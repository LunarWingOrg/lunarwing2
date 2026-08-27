# Reflex Compiler System — Comprehensive Analysis

**Date:** July 23, 2026  
**Codebase:** LunarWing v2 (`/home/openjaw/lunarwing2`)  
**Purpose:** Deep-dive analysis of the reflex compiler system for Christopher (project owner) to assess current state and identify gaps after the v1→v2 engine migration.

---

## 0. Status Update (2026-07-31, v2.0.4.0, branches `rarity/kimi-5-20260730-1201` + `aj/kimi-6-20260731-0902`)

The wiring gaps in §5 were addressed (first pass adapted from branch `anattemptatreflexcompilerv2`; second pass review fixes on `aj/kimi-6-20260731-0902`):

| Gap | Status |
|-----|--------|
| 5.1 Embedding provider never wired | **FIXED** — `Agent::new()` attaches the workspace provider, but `REFLEX_SEMANTIC_ENABLED` is a separate default-off opt-in and cache state is checked before provider I/O |
| 5.2 `bump_reflex_pattern_match()` never called | **FIXED** — successful routes synchronously merge canonical owner-scoped hits into a bounded accumulator; one worker writes additive database batches |
| 5.3 `ReflexEvictor` never spawned | **FIXED** — spawned in `Agent::run()` with `EvictionConfig` from `ReflexConfig` |
| 5.4 Hardcoded `"default"` user_id | **FIXED** — `ReflexCompiler` takes `user_id`; `find_recurring_job_patterns` filters per-user in both backends |
| 5.5 `compilation_attempts` never incremented | **FIXED** (2nd pass) — first-pass `increment_reflex_compilation_attempts` was a 0-row UPDATE (rows only exist after a successful upsert); replaced with `record_reflex_compilation_failure` upsert that inserts `status='failed'` rows and auto-disables after 3 attempts; successful retry reactivates with the new tool name and resets the counter |
| 5.6 No v2 safety/hooks/approval on reflex path | OPEN — architectural (intentional fast-path); needs design decision |
| 5.7 `FuzzyConfig` defined but unused | **FIXED** — wired into `ReflexRouter` (`with_fuzzy_config`, router fields) |
| 5.8 Missing config options | **FIXED** — `fuzzy_threshold`, default-off `semantic_enabled`, `semantic_threshold`, eviction settings, and `max_patterns_per_user` have env overrides; the per-user cap is enforced by the compiler |
| 5.9 libSQL boolean divergence | OPEN — documented, correct as-is |
| 5.10 PG prune re-query missing embedding columns | **FIXED** |
| 5.11 No engine v2 bridge adapter | OPEN — large architectural effort |
| 5.12 Dead code / unused exports | **FIXED** — `FuzzyConfig`, `EvictionConfig`, `spawn_reflex_evictor`, semantic search are now all live |

Additional second-pass fix (not in original gap list): `compile_pattern` only skipped `status='active'`, so user-disabled patterns (CLI `reflex delete`) and attempt-capped patterns were recompiled and resurrected on the next sweep. The compiler now skips `disabled` too; `failed`/`evicted` still fall through to retry.

Third-pass fix (2026-08-01, branch `aj/kimi-6-20260801-0002`, adopts `aj/kimi-6-20260731-2101` incl. the owner-scoped bump fix): `semantic_route()` and `try_route_with_details()` held `pattern_embeddings.read` while acquiring `patterns.read`, the inverse of `refresh()`'s `patterns.write` → `embeddings.write` order — a latent ABBA deadlock that becomes reachable in production once gap 5.1 wires the embedding provider. The embeddings guard is now scoped to the best-match computation and dropped before touching `patterns`; `try_route_with_details` also drops its fuzzy-scan `patterns` guard before the semantic fallback re-acquires it.

Fourth-pass fix (2026-08-02): attaching an embedding provider no longer implicitly enables semantic matching. `REFLEX_SEMANTIC_ENABLED` defaults off, and an empty pattern-vector cache is checked before query embedding. Per-route PostgreSQL/libSQL writes were replaced with a bounded write-behind accumulator and one periodic worker. Failed/cancelled batches requeue with capped exponential backoff; PostgreSQL uses one additive `UNNEST` update, libSQL uses one prepared transaction, and shutdown attempts a bounded final flush.

Verification for the fourth pass includes semantic no-provider-call regressions, accumulator cap/requeue/backoff tests, libSQL additive batch/empty/duplicate/final-flush coverage, both backend compile surfaces, and the scoped full checks recorded with release item 7.

Fifth-pass fix (2026-08-03, commit `20890027`): a hit buffered in the write-behind accumulator can land after the eviction transaction swept its pattern. Such a hit proves the cached pattern was live, so the additive `bump_reflex_pattern_match` now reactivates an `evicted` row back to `active` instead of leaving it evicted; covered by a late-hit-after-eviction regression in both backends.

---

## 1. What the Reflex Compiler Does

### Purpose

The reflex compiler is a **fast-path execution system** that detects recurring user prompts and compiles them into optimized WASM micro-skills, **bypassing the LLM entirely** for known patterns. This provides sub-100ms responses for recurring tasks versus multi-second LLM latency.

### Architecture Overview

```
User Input
    │
    ▼
[ReflexRouter] ──(fast path)──▶ [Compiled WASM Tool] ──▶ Response
    │ (miss)
    ▼
[Normal LLM Path]
    │
    ▼
[Background ReflexCompiler] ──(periodic scan)──▶ [New WASM Tools]
```

### Trigger Type

The reflex system has a **single trigger type**: recurring job descriptions. The background `ReflexCompiler` queries the `agent_jobs` table for descriptions that appear ≥ `min_match_count` times with `status = 'completed'` and `success = true`.

**Key query** (`find_recurring_job_patterns`, `reflex.rs:648-666`):
- Groups completed, successful jobs by `description`
- `HAVING COUNT(*) >= min_match_count`
- Returns up to `max_patterns_per_run` patterns ordered by frequency

### Action Type

The single action is **WASM tool compilation via `SoftwareBuilder`**:
- Generates a `BuildRequirement` with `SoftwareType::WasmTool`, `Language::Rust`
- Tool name: `reflex_{uuid_simple}` (e.g., `reflex_a1b2c3d4...`)
- Capabilities: `["log"]` (minimal)
- On success: persists pattern → tool mapping to `reflex_patterns` table
- On failure: logs warning, does not persist (no retry counter increment — see Gaps §5)

### Matching Strategy (3-Tier)

The `ReflexRouter` (`reflex.rs:125-567`) uses a cascading match strategy:

| Tier | Method | Mechanism | Performance |
|------|--------|-----------|-------------|
| 1 | **Exact match** | O(1) hash lookup on normalized input | Fastest |
| 2 | **Fuzzy match** | Jaro-Winkler similarity + word-overlap pre-filter, threshold 0.85 | Bounded scan, max 50 candidates |
| 3 | **Semantic match** | Embedding cosine similarity, threshold 0.75 (requires `EmbeddingProvider`) | Only when provider configured |

**Auto-promotion**: Frequently-matched fuzzy hits (≥3 hits for the same input variant) are promoted to the exact-match cache for O(1) routing.

**Normalization** (`reflex.rs:50-62`): lowercase → strip non-alphanumeric/non-whitespace → collapse whitespace.

---

## 2. Key Types and Data Structures

### Core Rust Types

#### `ReflexRouter` (`reflex.rs:125-139`)
```rust
pub struct ReflexRouter {
    patterns: Arc<RwLock<HashMap<String, String>>>,      // normalized → tool_name
    fuzzy_hits: Arc<RwLock<HashMap<String, u32>>>,       // input variant → hit count
    fuzzy_threshold: f64,                                 // default: 0.85
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    semantic_threshold: f64,                              // default: 0.75
    pattern_embeddings: Arc<RwLock<HashMap<String, Vec<f32>>>>, // pattern → embedding
}
```

#### `ReflexCompiler` (`reflex.rs:606-612`)
```rust
pub struct ReflexCompiler {
    builder: Arc<dyn SoftwareBuilder>,
    store: Arc<dyn Database>,
    check_interval: Duration,
    min_match_count: u32,
    max_patterns_per_run: usize,
}
```

#### `FuzzyMatch` (`reflex.rs:109-117`)
```rust
pub struct FuzzyMatch {
    pub tool_name: String,
    pub matched_pattern: String,
    pub score: f64,
}
```

#### `ReflexEvictor` (`reflex.rs:758-761`)
```rust
pub struct ReflexEvictor {
    store: Arc<dyn Database>,
    config: EvictionConfig,
}
```

#### `EvictionConfig` (`reflex.rs:734-740`)
```rust
pub struct EvictionConfig {
    pub check_interval: Duration,   // default: 24h
    pub stale_after_days: i32,      // default: 30
}
```

#### `ReflexPatternRecord` (`db/mod.rs:813-828`)
```rust
pub struct ReflexPatternRecord {
    pub id: Uuid,
    pub user_id: String,
    pub normalized_pattern: String,
    pub original_pattern: String,
    pub tool_name: String,
    pub match_count: i32,
    pub last_matched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status: String,                 // "active" | "disabled" | "evicted"
    pub compilation_attempts: i32,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model: Option<String>,
}
```

#### `ReflexConfig` (`config/agent.rs:44-55`)
```rust
pub struct ReflexConfig {
    pub enabled: bool,                    // default: false
    pub check_interval: Duration,         // default: 1800s (30 min)
    pub min_match_count: u32,             // default: 3
    pub max_patterns_per_run: usize,      // default: 5
}
```

### Database Schema

#### `reflex_patterns` table (migration V19)
- `id UUID PRIMARY KEY`
- `user_id TEXT NOT NULL`
- `normalized_pattern TEXT NOT NULL`
- `original_pattern TEXT NOT NULL`
- `tool_name TEXT NOT NULL`
- `match_count INTEGER DEFAULT 1`
- `last_matched_at TIMESTAMPTZ`
- `created_at / updated_at TIMESTAMPTZ`
- `status TEXT DEFAULT 'active'` (`active`, `disabled`, `evicted`)
- `compilation_attempts INTEGER DEFAULT 0`
- `UNIQUE(user_id, normalized_pattern)`
- Indexes: `(user_id, status)`, `(user_id, match_count DESC)`, `(tool_name)`

#### Embedding columns (migration V20)
- `embedding vector` (pgvector, no fixed dimension)
- `embedding_model TEXT`

### `ReflexStore` Trait (`db/mod.rs:735-810`)

| Method | Purpose |
|--------|---------|
| `find_recurring_job_patterns` | Scan `agent_jobs` for descriptions ≥ N occurrences |
| `upsert_reflex_pattern` | Insert or bump match count |
| `get_reflex_pattern` | Lookup by (user_id, normalized_pattern) → (tool_name, status) |
| `list_reflex_patterns` | List all patterns for a user |
| `disable_reflex_pattern` | Soft-delete (set status='disabled') |
| `bump_reflex_pattern_match` | Increment match_count + update last_matched_at |
| `prune_stale_reflex_patterns` | Evict patterns older than N days |
| `update_reflex_pattern_embedding` | Store embedding vector + model name |
| `semantic_search_reflex_patterns` | pgvector `<=>` cosine distance search (PG) / in-memory cosine (libSQL) |

---

## 3. Data Flow — From Trigger to Execution

### Compilation Flow (Background)

```
1. spawn_reflex_compiler()          [agent_loop.rs:747]
   └─▶ ReflexCompiler::run_loop()   [reflex.rs:633-642]
       └─▶ (every check_interval)
           compile_patterns()       [reflex.rs:648-666]
           │
           ├─ store.find_recurring_job_patterns(min_count, max_per_run)
           │   → Vec<String> (job descriptions)
           │
           └─ for each description:
               compile_pattern(description)   [reflex.rs:669-727]
               │
               ├─ normalize_pattern(description)
               ├─ store.get_reflex_pattern("default", &normalized)
               │   → skip if already "active"
               ├─ BuildRequirement { name: "reflex_{uuid}", ... }
               ├─ builder.build(&req).await
               │   ├─ Ok(success) → store.upsert_reflex_pattern(...)
               │   ├─ Ok(failed) → log warning (no retry increment!)
               │   └─ Err(e)     → log error
               └─ done
```

### Cache Refresh Flow (Background)

```
2. spawn_reflex_cache_refresh()     [agent_loop.rs:764-769]
   └─▶ (every check_interval)
       ReflexRouter::refresh()      [reflex.rs:454-481]
       │
       ├─ store.list_reflex_patterns(user_id)
       ├─ patterns.clear(); embeddings.clear()
       ├─ for active records:
       │   patterns.insert(normalized, tool_name)
       │   if embedding: embeddings.insert(normalized, emb)
       └─ if embedding_provider:
           embed_missing_patterns()  [reflex.rs:484-534]
           └─ provider.embed_batch(missing)
               └─ store.update_reflex_pattern_embedding(...)
```

### Routing Flow (Hot Path — Per Message)

```
3. IncomingMessage arrives at dispatcher.rs:122
   │
   ▼
   self.reflex_router.try_route(&message.content)  [reflex.rs:182-211]
   │
   ├─ Tier 1: Exact match (HashMap lookup on normalized input)
   │   └─ hit → return tool_name
   │
   ├─ Tier 2: Fuzzy match  [reflex.rs:214-294]
   │   ├─ word_overlap pre-filter (Jaccard ≥ 0.3 OR any word intersection)
   │   ├─ Jaro-Winkler scoring (boost to 0.9 if all pattern words contained)
   │   ├─ filter ≥ fuzzy_threshold (0.85)
   │   ├─ select best candidate
   │   └─ bump_fuzzy_hit() → promote to exact cache after 3 hits
   │       hit → return tool_name
   │
   └─ Tier 3: Semantic match  [reflex.rs:297-337]
       ├─ provider.embed(normalized)
       ├─ cosine_similarity against all cached embeddings
       ├─ filter ≥ semantic_threshold (0.75)
       └─ hit → return tool_name
   
   │
   ├─ HIT: execute_chat_tool(tool_name, {"input": message.content})
   │   └─ Ok(output) → return AgenticLoopResult::Response(output)
   │   └─ Err(e) → log warning, FALL BACK to LLM path
   │
   └─ MISS: continue to normal LLM agentic loop
```

### Eviction Flow

```
4. spawn_reflex_evictor()  [reflex.rs:856-864]  ← **NOT CALLED IN PRODUCTION** (see Gaps §5.3)
   └─▶ ReflexEvictor::run_loop()
       └─▶ (every 24h)
           sweep()  [reflex.rs:782-811]
           └─ store.prune_stale_reflex_patterns(30, false)
               → patterns with last_matched_at < now-30d set to 'evicted'
```

---

## 4. Integration Points with the v2 Engine

### Current Integration

The reflex system is integrated into the **v1 agent loop architecture** (`agent_loop.rs`, `dispatcher.rs`). The key integration points are:

#### 4.1 Agent Struct (`agent_loop.rs:274-275`)
```rust
pub struct Agent {
    ...
    pub(super) reflex_router: Arc<crate::agent::reflex::ReflexRouter>,
}
```
Initialized in `Agent::new()` at line 345 as `ReflexRouter::new()` — **no embedding provider is attached**.

#### 4.2 Dispatcher Fast-Path (`dispatcher.rs:121-143`)
The reflex check happens in `run_agentic_loop()` **before** the LLM is invoked:
- Checks `self.reflex_router.try_route(&message.content)`
- On hit: creates a `JobContext`, executes the tool via `execute_chat_tool()`
- On failure: falls back to LLM (graceful degradation)
- The reflex path does **not** go through the agentic loop's tool approval, hooks, or cost tracking

#### 4.3 Background Tasks (`agent_loop.rs:738-777`)
Two background tasks are spawned during `Agent::run()`:
1. **Reflex compiler** — `spawn_reflex_compiler()` (lines 747-753)
2. **Cache refresh** — `spawn_reflex_cache_refresh()` (lines 764-769)

Both are gated by `self.config.reflex.enabled` (default: `false`).

#### 4.4 CLI Interface (`main.rs:144-158`, `cli/reflex.rs`)
The `lunarwing reflex` command provides offline management:
- `list`, `show`, `delete`, `status`, `prune`

#### 4.5 Engine V2 Bridge — **NO INTEGRATION**

The `src/bridge/` module (14 files) implements the v2 engine bridge (SSH, LLM adapter, store adapter, router, etc.). **There is zero integration between the reflex system and the bridge/engine v2.** Specifically:
- The `mission_manager_slot` (`agent_loop.rs:272-273`) has no connection to reflexes
- No bridge adapter exists for reflex patterns
- The reflex router is not exposed to the v2 engine's effect/observation system

---

## 5. GAPS: Outdated, Broken, or Diverged from v2 Patterns

### 5.1 CRITICAL: Embedding Provider Never Wired to Router

**File:** `agent_loop.rs:345`
```rust
reflex_router: Arc::new(crate::agent::reflex::ReflexRouter::new()),
```

The `ReflexRouter` is always created with `ReflexRouter::new()`, which sets `embedding_provider: None`. The `with_embedding_provider()` builder method exists and works (tested at `reflex.rs:1578-1581`), but **it is never called in production code**. This means:

- **Tier 3 (semantic matching) is permanently disabled in production**
- The `semantic_route()` method, `pattern_embeddings` cache, `embed_missing_patterns()`, and the DB's `semantic_search_reflex_patterns()` + `update_reflex_pattern_embedding()` are all dead code in production
- The workspace's `EmbeddingProvider` (which exists and is used for workspace search) is not shared with the reflex router

**Fix:** In `Agent::new()` or `Agent::run()`, attach the workspace embedding provider:
```rust
let router = ReflexRouter::new();
if let Some(ws) = &deps.workspace {
    if let Some(provider) = ws.embedding_provider() {
        router = router.with_embedding_provider(provider);
    }
}
```

### 5.2 CRITICAL: `bump_reflex_pattern_match()` Never Called in Hot Path

**File:** `dispatcher.rs:122-143`

When the reflex router matches a pattern (exact, fuzzy, or semantic), **`bump_reflex_pattern_match()` is never called**. The `try_route()` method returns the tool name, but the dispatcher does not record the match back to the database. This means:
- `match_count` and `last_matched_at` in `reflex_patterns` are never updated during routing
- The eviction sweep's staleness check (`last_matched_at < cutoff`) will evict patterns that are actively being used
- The `match_count` field only reflects compilation-time counts from `find_recurring_job_patterns`, not actual routing hits

The `bump_reflex_pattern_match()` method exists in both backends and is tested (`reflex.rs:1336-1340`), but is never invoked from the dispatcher.

**Fix:** After a successful reflex route in `dispatcher.rs`, call:
```rust
if let Some(store) = self.store() {
    let _ = store.bump_reflex_pattern_match(&message.user_id, &normalized).await;
}
```

### 5.3 HIGH: ReflexEvictor Never Spawned

**File:** `agent_loop.rs:738-777`

The background task spawn block creates the compiler and cache refresh tasks, but **`spawn_reflex_evictor()` is never called**. The `ReflexEvictor` struct and `spawn_reflex_evictor()` function exist (`reflex.rs:758-864`) and are tested, but production code only spawns the compiler + cache refresh.

Stale patterns will accumulate indefinitely. The only way to evict them is manually via `lunarwing reflex prune --stale-days N`.

**Fix:** Add evictor spawn in `Agent::run()`:
```rust
let _reflex_evictor_handle = if self.config.reflex.enabled {
    if let Some(store) = self.store() {
        Some(crate::agent::reflex::spawn_reflex_evictor(
            Arc::clone(store),
            crate::agent::reflex::EvictionConfig::default(),
        ))
    } else { None }
} else { None };
```

### 5.4 HIGH: Hardcoded `"default"` User ID in Compiler

**File:** `reflex.rs:673, 705`

The `ReflexCompiler::compile_pattern()` method hardcodes user_id as `"default"`:
```rust
match self.store.get_reflex_pattern("default", &normalized).await {  // line 673
...
self.store.upsert_reflex_pattern("default", &normalized, description, &safe_name).await  // line 705
```

This means:
- Patterns are only ever compiled for user `"default"`
- Multi-user deployments will not compile patterns for non-default users
- The `find_recurring_job_patterns()` query does not filter by user_id either (it groups all completed jobs globally)

This is a **v1-era design assumption** (single-user) that diverges from v2's multi-tenant architecture.

### 5.5 MEDIUM: `compilation_attempts` Never Incremented

**File:** `reflex.rs:715-721`

When a build fails, the compiler logs a warning but **does not increment `compilation_attempts`**:
```rust
} else {
    tracing::warn!(
        "Failed to compile Reflex MS for pattern: {} (error: {:?})",
        description, result.error
    );
    // ← No store call to increment compilation_attempts!
}
```

The `compilation_attempts` column exists in the schema and `ReflexPatternRecord` but is always 0. The proposal doc (`proposals/reflex-compiler.md:267`) says "Limit to 3 attempts, then disable" — this was never implemented.

### 5.6 MEDIUM: Reflexer Router Does Not Go Through V2 Safety/Hooks/Approval

**File:** `dispatcher.rs:122-143`

The reflex fast-path executes the compiled tool via `execute_chat_tool()` directly, which bypasses:
- The v2 agentic loop's 3-phase tool execution (preflight → parallel exec → post-flight)
- Hook pre/post-execution callbacks
- Approval flow (`ApprovalContext`)
- Cost tracking / `CostGuard` checks
- Skill-based tool attenuation

This is architecturally intentional (speed is the point), but means a compiled reflex tool has **no safety guardrails** beyond whatever the WASM sandbox provides. If the v2 engine's security model requires hooks or approval for certain operations, reflexes could bypass them.

### 5.7 MEDIUM: `FuzzyConfig` Defined But Unused

**File:** `reflex.rs:578-600`

The `FuzzyConfig` struct is defined with `threshold`, `promotion_threshold`, and `max_candidates` fields, but it is **never used to configure the router**. The router uses hardcoded constants (`DEFAULT_FUZZY_THRESHOLD`, `PROMOTION_THRESHOLD`, `MAX_FUZZY_CANDIDATES`). `FuzzyConfig` exists only for documentation/test purposes.

### 5.8 MEDIUM: Missing Config Options

**File:** `config/agent.rs:44-55`

`ReflexConfig` is missing several options that the code supports:
- `fuzzy_threshold` — hardcoded at 0.85, not configurable via env
- `semantic_threshold` — hardcoded at 0.75, not configurable via env
- `eviction_check_interval` — hardcoded at 24h in `EvictionConfig::default()`
- `eviction_stale_days` — hardcoded at 30 in `EvictionConfig::default()`
- `max_patterns_per_user` — the proposal mentions a 50-pattern cap, never implemented

### 5.9 LOW: libSQL `find_recurring_job_patterns` Uses `success = 1` (SQLite Boolean)

**File:** `db/libsql/reflex.rs:85-86`
```sql
WHERE status = 'completed' AND success = 1
```

vs PostgreSQL (`db/postgres.rs:1252`):
```sql
WHERE status = 'completed' AND success = true
```

This is correct for SQLite (no native boolean type) but should be documented as a known divergence.

### 5.10 LOW: Postgres `prune_stale_reflex_patterns` Re-Query Missing Embedding Columns

**File:** `db/postgres.rs:1388-1396`

After evicting, the Postgres backend re-queries to refresh status, but the re-query SELECT list **omits `embedding` and `embedding_model` columns** (line 1390):
```sql
SELECT id, user_id, normalized_pattern, original_pattern, tool_name,
       match_count, last_matched_at, created_at, updated_at, status, compilation_attempts
       -- missing: embedding, embedding_model
FROM reflex_patterns WHERE id = ANY($1)
```

The `row_to_reflex_pattern()` function uses `try_get` for these columns, so it won't crash, but the returned records will have `embedding: None` and `embedding_model: None` after eviction — even though the data still exists in the DB.

### 5.11 LOW: No Engine V2 Bridge for Reflexes

The `src/bridge/` module has adapters for SSH, LLM, store, effects, skills, and routing. There is **no reflex bridge adapter**. If the v2 engine needs to:
- Observe reflex matches as events
- Route reflexes through the v2 mission/gate system
- Track reflex tool execution in the v2 cost/observability pipeline

...none of that is possible without a bridge adapter.

### 5.12 LOW: Dead Code / Unused Exports

- `FuzzyConfig` (`reflex.rs:578`) — defined, tested, but never instantiated in production
- `EvictionConfig` (`reflex.rs:734`) — used only in tests since evictor is never spawned
- `spawn_reflex_evictor` (`reflex.rs:856`) — exported from `mod.rs` but never called
- `semantic_search_reflex_patterns` — implemented in both DB backends, never called outside tests
- `try_route_with_details()` (`reflex.rs:365`) — public method for diagnostics, never called in production

---

## 6. Key File Paths and Line Numbers

### Core Module
| What | File | Lines |
|------|------|-------|
| Module doc + matching strategy | `ic/src/agent/reflex.rs` | 1-14 |
| `normalize_pattern()` | `ic/src/agent/reflex.rs` | 50-62 |
| `word_overlap()` | `ic/src/agent/reflex.rs` | 68-82 |
| `cosine_similarity()` | `ic/src/agent/reflex.rs` | 85-104 |
| `FuzzyMatch` struct | `ic/src/agent/reflex.rs` | 109-117 |
| `ReflexRouter` struct | `ic/src/agent/reflex.rs` | 125-139 |
| `ReflexRouter::new()` | `ic/src/agent/reflex.rs` | 143-152 |
| `ReflexRouter::with_embedding_provider()` | `ic/src/agent/reflex.rs` | 167-170 |
| `ReflexRouter::try_route()` | `ic/src/agent/reflex.rs` | 182-211 |
| `ReflexRouter::fuzzy_route()` | `ic/src/agent/reflex.rs` | 214-294 |
| `ReflexRouter::semantic_route()` | `ic/src/agent/reflex.rs` | 297-337 |
| `ReflexRouter::bump_fuzzy_hit()` | `ic/src/agent/reflex.rs` | 341-362 |
| `ReflexRouter::try_route_with_details()` | `ic/src/agent/reflex.rs` | 365-451 |
| `ReflexRouter::refresh()` | `ic/src/agent/reflex.rs` | 454-481 |
| `ReflexRouter::embed_missing_patterns()` | `ic/src/agent/reflex.rs` | 484-534 |
| `FuzzyConfig` (unused in prod) | `ic/src/agent/reflex.rs` | 578-600 |
| `ReflexCompiler` struct | `ic/src/agent/reflex.rs` | 606-612 |
| `ReflexCompiler::run_loop()` | `ic/src/agent/reflex.rs` | 633-642 |
| `ReflexCompiler::compile_patterns()` | `ic/src/agent/reflex.rs` | 648-666 |
| `ReflexCompiler::compile_pattern()` (hardcoded "default") | `ic/src/agent/reflex.rs` | 669-727 |
| `EvictionConfig` | `ic/src/agent/reflex.rs` | 734-749 |
| `ReflexEvictor` | `ic/src/agent/reflex.rs` | 758-812 |
| `spawn_reflex_compiler()` | `ic/src/agent/reflex.rs` | 817-834 |
| `spawn_reflex_cache_refresh()` | `ic/src/agent/reflex.rs` | 837-853 |
| `spawn_reflex_evictor()` (never called in prod) | `ic/src/agent/reflex.rs` | 856-864 |

### DB Layer
| What | File | Lines |
|------|------|-------|
| `ReflexStore` trait definition | `ic/src/db/mod.rs` | 735-810 |
| `ReflexPatternRecord` struct | `ic/src/db/mod.rs` | 813-828 |
| `Database` supertrait (includes ReflexStore) | `ic/src/db/mod.rs` | 1270-1286 |
| libSQL `ReflexStore` impl | `ic/src/db/libsql/reflex.rs` | 73-334 |
| PG `ReflexStore` impl | `ic/src/db/postgres.rs` | 1242-1450 |
| PG `row_to_reflex_pattern()` | `ic/src/db/postgres.rs` | 1452-1469 |

### Integration Points
| What | File | Lines |
|------|------|-------|
| Agent struct `reflex_router` field | `ic/src/agent/agent_loop.rs` | 274-275 |
| Agent::new() router init (no embedding provider) | `ic/src/agent/agent_loop.rs` | 345 |
| Background compiler + cache spawn | `ic/src/agent/agent_loop.rs` | 738-777 |
| Dispatcher fast-path check | `ic/src/agent/dispatcher.rs` | 121-143 |
| Module exports | `ic/src/agent/mod.rs` | 42 |

### Config
| What | File | Lines |
|------|------|-------|
| `ReflexConfig` struct | `ic/src/config/agent.rs` | 44-55 |
| `ReflexConfig::default()` | `ic/src/config/agent.rs` | 57-66 |
| `ReflexConfig` env var resolution | `ic/src/config/agent.rs` | 163-171 |

### CLI
| What | File | Lines |
|------|------|-------|
| `ReflexCommand` enum | `ic/src/cli/reflex.rs` | 15-61 |
| `run_reflex_command()` | `ic/src/cli/reflex.rs` | 64-80 |
| CLI dispatch in main.rs | `ic/src/main.rs` | 144-158 |

### Migrations
| What | File |
|------|------|
| `reflex_patterns` table | `ic/migrations/V19__reflex_patterns.sql` |
| Embedding columns | `ic/migrations/V20__reflex_embeddings.sql` |

### Tests
| What | File |
|------|------|
| Unit tests (normalization, routing, fuzzy, semantic, DB CRUD) | `ic/src/agent/reflex.rs:868-1710` |
| E2E integration tests (compile → route → evict) | `ic/tests/e2e_reflex_compiler.rs` |

### Design Doc
| What | File |
|------|------|
| Original proposal | `docs/internal/history/proposals/reflex-compiler.md` |

---

## 7. Summary Assessment

### What Works Well
- **Three-tier matching** (exact → fuzzy → semantic) is well-designed with good pre-filtering
- **Auto-promotion** of frequent fuzzy hits to exact cache is clever and effective
- **Graceful fallback** — reflex failures silently fall back to the LLM path
- **Both DB backends** (PostgreSQL + libSQL) fully implement the `ReflexStore` trait
- **CLI** is complete and functional (list, show, delete, status, prune)
- **Test coverage** is solid: 30+ unit tests + 7 E2E integration tests
- **Eviction system** is well-designed with dry-run support

### What Needs Immediate Attention

| Priority | Issue | Impact |
|----------|-------|--------|
| 🔴 CRITICAL | Embedding provider never wired to router | Semantic matching (tier 3) is dead in production |
| 🔴 CRITICAL | `bump_reflex_pattern_match()` never called in dispatcher | Match counts/staleness tracking broken; active patterns will be evicted |
| 🟠 HIGH | `ReflexEvictor` never spawned | Stale patterns accumulate forever |
| 🟠 HIGH | Hardcoded `"default"` user_id | Multi-tenant broken; patterns only compile for default user |
| 🟡 MEDIUM | `compilation_attempts` never incremented | No retry limiting; failed patterns retry forever |
| 🟡 MEDIUM | No v2 safety/hooks/approval on reflex path | Bypasses v2 security model |
| 🟡 MEDIUM | Missing config options | Fuzzy/semantic thresholds, eviction params not configurable |
| 🔵 LOW | PG re-query missing embedding columns | Minor data inconsistency after eviction |
| 🔵 LOW | No engine v2 bridge adapter | Cannot integrate with v2 observability/gate system |

### Overall Verdict

The reflex compiler was **well-architected in v1** but has been **neglected during the v2 migration**. The core matching logic is sound and tested, but several critical wiring gaps prevent it from functioning correctly in production:

1. **Semantic matching is wired in code but never activated** (embedding provider not connected)
2. **Routing doesn't feedback to the database** (match tracking broken)
3. **The evictor exists but is never started** (stale pattern cleanup broken)
4. **Multi-user support was not updated** (hardcoded "default" user_id)
5. **No v2 bridge integration** (reflexes bypass v2 security/observability)

The system is **disabled by default** (`REFLEX_COMPILER_ENABLED=false`), so these issues are latent — they won't affect users until someone enables the feature. But enabling it in its current state would result in a partially-functional system with broken staleness tracking and no semantic matching.
