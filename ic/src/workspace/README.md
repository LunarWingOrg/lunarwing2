# Workspace & Memory System

Inspired by [OpenClaw](https://github.com/openclaw/openclaw), the workspace provides persistent memory for agents with a flexible filesystem-like structure.

## Key Principles

1. **"Memory is database, not RAM"** - If you want to remember something, write it explicitly
2. **Flexible structure** - Create any directory/file hierarchy you need
3. **Self-documenting** - Use README.md files to describe directory structure
4. **Hybrid search** - Combines FTS (keyword) + vector (semantic) via Reciprocal Rank Fusion

## Filesystem Structure

```
workspace/
├── README.md              <- Root runbook/index
├── MEMORY.md              <- Long-term curated memory
├── HEARTBEAT.md           <- Periodic checklist
├── IDENTITY.md            <- Agent name, nature, vibe
├── SOUL.md                <- Core values
├── AGENTS.md              <- Behavior instructions
├── USER.md                <- User context
├── TOOLS.md               <- Environment-specific tool notes
├── BOOTSTRAP.md           <- First-run ritual (deleted after onboarding)
├── context/               <- Identity-related docs
│   ├── vision.md
│   └── priorities.md
├── daily/                 <- Daily logs
│   ├── 2024-01-15.md
│   └── 2024-01-16.md
├── lorebooks/             <- Bounded SillyTavern-style world-lore JSON
│   └── fantasy.json
├── projects/              <- Arbitrary structure
│   └── alpha/
│       ├── README.md
│       └── notes.md
├── engine/.runtime/       <- Internal Engine recovery state (persisted, not search-indexed)
├── engine/projects/       <- Mission goals under */missions/*/ (persisted, not search-indexed)
└── ...
```

## Using the Workspace

```rust
use std::sync::Arc;
use crate::workspace::{Workspace, OpenAiEmbeddings, paths};

// Create workspace for a user (wraps embeddings in a default LRU cache)
let workspace = Workspace::new("user_123", pool)
    .with_embeddings(Arc::new(OpenAiEmbeddings::new(api_key)));

// For tests: skip the cache layer (avoids unnecessary overhead with mocks)
// let workspace = Workspace::new("user_123", pool)
//     .with_embeddings_uncached(Arc::new(MockEmbeddings::new(1536)));

// Read/write any path
let doc = workspace.read("projects/alpha/notes.md").await?;
workspace.write("context/priorities.md", "# Priorities\n\n1. Feature X").await?;
workspace.append("daily/2024-01-15.md", "Completed task X").await?;

// Convenience methods for well-known files
workspace.append_memory("User prefers dark mode").await?;
workspace.append_daily_log("Session note").await?;

// List directory contents
let entries = workspace.list("projects/").await?;

// Search (hybrid FTS + vector)
let results = workspace.search("dark mode preference", 5).await?;

// Get system prompt from identity files
let prompt = workspace.system_prompt().await?;
```

### Prompt-Injection Write Scanning

Writes to prompt-visible files (the identity files, `lorebooks/`, and
`daily/*.md`) are scanned for prompt-injection patterns before storage and
rejected on High-or-above matches; appends re-scan the full candidate so
patterns split across appends cannot slip through. This includes the
`append_memory()` lane behind `memory_write(target: "memory", append: true)`
(WI#286 — it previously bypassed the scan entirely). Daily logs accumulate
agent-authored incident and test notes, so their scan demotes a match fully
enclosed in a short, single-line, inline quoted span (`` `code` ``,
`"double"`, `'single'`, `“curly”`, `‘curly’`, at most 200 bytes) to warn-only
describing context (WI#280). A closing quote that fails its right-boundary
check ends the span instead of keeping it open, so mismatched quotes cannot
be eagerly re-paired to bracket injection text (`"a"ignore previous …"`
stays unquoted and rejects). Unquoted matches and tokenizer-level patterns
(`<|`, `|>`, `[INST]`, `[/INST]`, null bytes) still reject in daily logs, and
all other prompt-visible files keep the strict raw scan. Rejection errors name
the offending file and the 1-based line of each rejecting match so an operator
can repair quickly (WI#288).

Rejections reach the agent as their own error class (WI#290): `memory_write`
maps `WorkspaceError::InjectionRejected` to `ToolError::ContentRejected`
(rendered `Content rejected: …`), not the permission-flavored `NotAuthorized`
that read like a transient gate and invited retrying the identical write. The
message carries the matched pattern descriptions with line numbers, states that
the same text is rejected on every retry, tells the agent to rephrase the
flagged wording, and warns against copying the error into memory — some
pattern descriptions trip the scan themselves. The echoed list is capped at
the first ten matches plus a count of the rest, so the remedy always survives
the tool-output cap (`max_output_length`, 100 KB by default); the HTTP/CLI
error text and the gateway's live log stream keep the full list, while
journal/stderr lines are cut at 500 bytes. The inline-quote route is offered
only for daily logs (the one lane where quoting demotes a match) and names
the reliable form: one backtick pair on a single line, at most 200 bytes
(`MAX_QUOTED_SPAN_BYTES`) between them, with no other quote mark left open
before it on that line (an earlier unbalanced quote swallows the backticks).
The hint also says that quoting neutralizes nothing: the phrase is stored
verbatim and stays model-visible in later system prompts — prompt assembly
escapes nothing on the v1 path and breaks only `<|` under Engine V2 — so
quote only what the record needs, never text meant to steer the assistant. The guidance and all three agent-facing
messages (injection rejection, existing-content rejection, daily-log
quarantine) are pinned scanner-clean with real pattern descriptions by a
regression test: every scanner hit lies inside the echoed list. Gate
strictness is unchanged.

An append whose entry passes the scan but whose file already holds content
that trips the gate on its own (typically `MEMORY.md` text stored through
`append_memory()` before WI#286 scanned that lane) fails with
`WorkspaceError::ExistingContentRejected` instead: `append()` on
prompt-visible files other than default-scope daily logs, `append_memory()`,
and `append_to_layer()` on any prompt-visible file (layer daily logs are not
quarantined). Rephrasing the entry cannot help, so `memory_write` maps it to
`ContentRejected` with escalate-first guidance: tell the user which lines need
repair; self-repair is allowed only when no layer was passed (`memory_read`
cannot address a layer, and `memory_write` without `layer` writes the default
scope), the file is short enough to rewrite exactly in one call, and every
flagged line is plainly an ordinary note rather than an instruction aimed at
the assistant — then read it with `memory_read` and replace it with
`memory_write` and `append: false`, keeping every other line unchanged and
rewording each flagged line without the flagged wording. A planted injection
is therefore never laundered past the scanner by "rephrasing the flagged
lines". The classification runs only after the combined scan has already
failed and only relabels the error: boundary-split patterns (stored content
clean on its own) and poisoned entries stay `InjectionRejected`, and the
append is refused either way. The web `POST /api/memory/write` endpoint
answers HTTP 422 for injection rejections (matching the prompt editor) and
HTTP 409 for existing-content rejections and daily-log quarantines, instead
of 500.

The gate is deliberately **stricter than the sanitizer's own neutralization**
(WI#289 — decision: document, not align). Detection matches role labels and
override phrasing anywhere in the text, so a mid-sentence `user:` rejects
exactly like a line-start one, while the sanitizer's render-time escaping only
prefixes *line-start* role labels (and only when a Critical pattern fires) and
never rewrites phrase patterns. Matching is overlapping: a shorter pattern
cannot hide an overlapping longer one, so `exact assistant:` reports both the
Medium `act as` and the High `assistant:` and rejects. The asymmetry stays
because, for these files, the write gate is the only keyword defense — prompt
assembly applies nothing (v1 engine) or only the `<|` opener break (Engine
V2) — and because positional matching is evadable (a zero-width character or
U+2028 hides a label from a line-start rule) while position-free matching has
no position to spoof.
Position-free is not obfuscation-proof, though: NFKC/format-character
canonicalisation runs only in the lorebook lane, so zero-width, format-char or
fullwidth variants of a phrase can still pass the raw scans of the other
prompt-visible lanes (a pre-existing gap this decision does not change). False
positives are handled by rephrasing (the rejection names the file, the matched
pattern descriptions and their 1-based lines) or, in daily logs, by inline
quoting. The full rationale lives in the `lunarwing_safety` sanitizer module
docs; tests in both places pin the matrix (`detect_vs_neutralize`,
`test_role_label_detection_is_position_free_in_every_prompt_file_class`,
`test_overlapping_shorter_match_cannot_smuggle_a_label_past_the_gate`).

### Daily-Log Quarantine (WI#288)

An append to a daily log re-scans the full combined candidate, so a day file
already holding *unquoted* High-severity content (e.g. written before the gate
existed) would otherwise reject every subsequent append and freeze the day's
log with no tool-level recovery. When the combined scan fails but the new
entry itself is clean and the pre-existing content alone trips the gate,
`append()` quarantines the day file aside to `daily/YYYY-MM-DD.poisoned-<n>.md`
(first free slot) and clears the day file. The discovering append is not
applied and fails with the distinct `WorkspaceError::DailyLogQuarantined`
(path, quarantine path, and the pre-existing matches) rather than an ordinary
injection rejection, because the remedy is the opposite one: the new entry was
clean, so `memory_write` tells the agent to retry it unchanged (a
retry-inviting `ExecutionFailed`, never `ContentRejected`), and the retry lands
on the fresh log; the web API answers HTTP 409 (WI#290). The quarantine
message echoes the pre-existing match descriptions (capped at ten plus a
count) and, like the rejection messages, warns against copying itself into
memory. The quarantine copy is retained with an empty chunk projection —
exact-path readable, listed, never search-indexed — and is never
prompt-injected (daily-log injection reads only the exact
`daily/YYYY-MM-DD.md` path). Quarantine applies only when the poison
pre-exists: a poisoned entry, or a pattern split across the append boundary,
is still a plain rejection with no file movement. Quarantine failures are
logged and never mask the rejection; the refused append is then relabelled
`ExistingContentRejected` (the stored content alone already failed the gate,
so rephrase advice would blame the clean entry).

For a deliberate repair, `memory_write(target: "daily_log", append: false)`
replaces today's log through `write()`: only the new content is scanned, and
the previous version (poisoned or not) is kept in the shadow backup
`backups/daily/YYYY-MM-DD.md.bak` (earlier generations roll aside to numbered
`.bak.<n>` slots). The named `daily_log` target used to ignore `append: false`
and append silently while reporting a replace (WI#288 residual); the
explicit-path form (`target: "daily/YYYY-MM-DD.md"`) always honored it. When a
`layer` is specified the write routes through the layer methods: replaces get
the same shadow backup, but appends do not quarantine — a poisoned layer-scope
day file rejects until it is replaced or repaired by an operator.

### Prompt-File Shadow Backups (WI#286)

A full-replace `write()` of a prompt-visible file destroys its prior content —
the mechanism behind the incident where a session's context dump clobbered the
curated `MEMORY.md`. Before replacing non-empty, differing content, `write()`
and `write_to_layer()` now persist the previous version to a shadow copy at
`backups/<path>.bak` in the written scope (e.g. `backups/MEMORY.md.bak`), and
every backup emits a `lunarwing::safety` warn audit line naming the path, the
prior byte count and the backup path. Most prompt-visible files keep a single
rolling slot, overwritten on each replace; daily logs additionally retain the
eight most recent earlier generations — before the slot is overwritten, its
content rolls aside to the first free `backups/daily/<date>.md.bak.<n>`, and at
the cap the oldest generation is discarded (with its own audit line) and the
rest shift down — so repeated same-day replaces (including the WI#288 repair
path) never silently discard a recent version, and retained storage stays
bounded. A replace retried after a crash between the backup and the replace
commit grows no duplicate generation. Backups are retrievable by exact-path
`read()` and visible in `list()`, but are stored with an empty chunk
projection so they never enter the search index. Because they can hold
quarantined injection content and are the recovery evidence for replaced
prompt files, the `backups/` prefix is reserved: the memory tools reject
writes and reads under it and hide it from tree listings, and the web
`POST /api/memory/write` route answers HTTP 403 for it (operator reads stay
open). Backup and rotation failures are logged and never block the write
itself.

## Operator Prompt Editing (WI#220)

The gateway's **Settings → System Prompt** panel edits the authenticated primary
scope's `AGENTS.md`, `SOUL.md`, `USER.md`, `IDENTITY.md`, `TOOLS.md`, `MEMORY.md`
and `BOOTSTRAP.md` through ordinary `Workspace::write()` validation/indexing.
`GET /api/prompts` lists those primary-scope sources;
`POST /api/prompts/write` saves one; `GET /api/prompts/preview` invokes
`system_prompt_for_context_tz()` with explicit group/timezone controls.

The preview shows current saved workspace composition. Engine V2 snapshots that
component at thread-manager initialization and retains assembled System messages
on existing threads. Activating edits there requires a tenant restart followed
by a new conversation. See the [operator guide](../../../docs/guides/TENANT-SYSTEM-PROMPTS.md)
for scope, preview, activation and API details.

## Memory Tools

Four tools for LLM use:

- **`memory_search`** - Hybrid search, MUST be called before answering questions about prior work
- **`memory_write`** - Write memory, daily logs, or custom user paths (payloads capped at 256 KiB, matching the web prompt editor)
- **`memory_read`** - Read bounded pages from a file by path
- **`memory_tree`** - View a bounded workspace tree (depth parameter, default 1)

`memory_search` excludes internal `engine/.runtime/**` documents, the
mission-directory subtree (`engine/projects/*/missions/*/**`, WI#135
hardening), and the project-level `missions.json` manifest, so mission goal
text stays out of the index even when written to adjacent artifacts like
`mission.json.bak`. Each
returned snippet is bounded to 2,000 JSON-encoded bytes, and the complete result
remains valid JSON bounded to 8,000 serialized bytes. Queries are limited to
1,024 UTF-8 bytes before embedding or database search. Other engine-internal
documents that legitimately appear in results (for example
`engine/knowledge/summaries/*`) carry an explicit `"provenance":
"engine_internal"` label so they are never silently ordinary-looking memory.

All agent-facing memory tools reserve `engine/.runtime/**` and the
shadow-backup prefix `backups/`: writes and direct reads are rejected, and
tree listings hide both subtrees. Direct `Workspace` APIs
remain available to the Engine for recovery. `memory_read` returns at most 6,000
source bytes per page plus `next_offset` and a revision required for later pages;
each response is at most 8,000 serialized bytes. `memory_tree` returns at most
256 entries and 8,000 serialized bytes with machine-readable `truncated`,
`entry_count`, `next_offset`, and root-list `revision` fields.

## Study-Notes Knowledge Store

Study notes are durable, source-verified codebase-study findings shared between
agents. They are stored as ordinary workspace documents under
`study-notes/<agent>/<topic>.md` (per-agent versions of a canonical topic) with
a `study-note` provenance tag plus structured metadata (agent attribution,
topic slug, confidence 0.0-1.0, status `active`/`stale`/`superseded`/
`needs-verification`, studied timestamp, source commit, tags array, per-note
`stale_after_days` threshold, `status_reason`) in the
document `metadata` JSONB. Because they are workspace documents, they flow
through the normal chunking and PostgreSQL FTS pipeline (`memory_chunks`
`content_tsv` GIN index, `ts_rank_cd` ranking). The tool surface and metadata
model match the canonical [lw2studymcp](https://git.lunarwing.org/cmc/lw2studymcp)
store.

Seven native tools (see `src/tools/builtin/knowledge.rs`):

- **`knowledge_search`** — PostgreSQL FTS over study-note chunks with optional
  topic/agent/status/min-confidence filters and every-listed-tag matching;
  best-ranked snippet per note.
- **`knowledge_get_topic`** — all per-agent versions of one topic with content.
- **`knowledge_list_topics`** — topic coverage survey: per-topic version
  counts, scopes, agents, statuses, and tags; optional substring `filter`.
- **`knowledge_submit_note`** — validated write (slug/topic/title/content
  limits, confidence 0-1, RFC 3339 `studied_at`, `stale_after_days` defaulting
  to the canonical 90 days, `status_reason`); `replace=true` updates the
  caller's own agent version only (other agents' versions live at different
  paths). `status=superseded` requires `superseded_by`.
- **`knowledge_update_status`** — metadata-only lifecycle update; only provided
  fields change, explicit null clears nullable fields, and leaving `superseded`
  without an explicit `superseded_by` clears it. Content changes go through
  `knowledge_submit_note` with `replace=true`.
- **`knowledge_list_stale`** — notes needing verification by status, age
  (`max_age_days`, falling back to the note's own `stale_after_days`), or
  `repository_commit` mismatch.
- **`knowledge_stats`** — totals, distinct topics, per-status and per-scope counts.

Scopes: reads span the caller's private scope plus the shared herd scope
(`workspace::KNOWLEDGE_SHARED_SCOPE`, `"shared"`) unless narrowed with
`scope=all|private|shared`. Writes default to the private scope;
`scope=shared` publishes herd knowledge.

### Lifecycle (Phase 2)

- **Provenance** — every submitted note records `submitted_by` (the real
  caller scope) and `session_id` (conversation/job) from the job context, in
  addition to the self-declared `agent` slug, so shared notes keep
  attribution. Legacy notes without these fields still parse.
- **Stale sweep** (`knowledge_sweep`) — a cadence-guarded background pass
  marks `active` notes `stale` once `studied_at` (falling back to the document
  update time) is older than `KNOWLEDGE_STALE_AFTER_DAYS` (default 30). The
  pass pages through the entire store with a keyset cursor (500-row pages,
  100k safety ceiling), so stores larger than any single query window are
  fully covered (WI#121). Cadence: `KNOWLEDGE_SWEEP_CADENCE_HOURS`
  (default 24); the shared herd scope is swept too unless
  `KNOWLEDGE_SWEEP_INCLUDE_SHARED=false`; the whole sweep can be disabled
  with `KNOWLEDGE_SWEEP_ENABLED=false`. It runs from two trigger paths:
  inside the heartbeat loop (single-node hosts) and from an independent
  daemon cadence spawned at startup (WI#121: multi-tenant renders set
  `HEARTBEAT_ENABLED=false`, so the heartbeat path alone never fires there).
  A global in-process guard plus a shared cadence state file keep the two
  paths from double-sweeping.
- **Kawarimi migration** — notes are ordinary `memory_documents` rows, so
  they travel with the tenant `pg_dump -Fc` → `pg_restore` bundle and the
  owner-scope rekey (see `docs/ops/MT-MACHINE-MIGRATION.md`).
- **Skill** — the bundled `study-notes` skill (v3) directs agents through
  these native tools; the external study-notes MCP is no longer required.

### New-tenant seed (Phase 3)

- **One-shot seed** (`knowledge_seed`) — multi-tenant provisioning
  (`lunarwing-mt-admin.sh add-tenant --with-study-mcp`) writes
  `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` into the new tenant's env. On startup the
  daemon imports every note from the synced lw2studymcp checkout
  (`$LW2STUDYMCP_HOME/agent-notes-by-agent/<agent>/<topic>.md`) into the
  tenant's private scope, preserving the canonical metadata (status,
  confidence, studied timestamp, source commit, tags, staleness thresholds).
  A JSON state file (`knowledge_seed_state.json` under the base dir, recording
  seeded/skipped/failed counts) makes the pass run at most once; note paths
  that already exist are always skipped, so the seed itself never repopulates
  the store and operator-authored notes are never overwritten. Per-note
  failures are logged and recorded in the state file without blocking the
  marker, so unimportable upstream notes cannot turn the one-shot seed into a
  per-boot rescan. The env flag is written only on the add-tenant path — never
  by `configure-study-mcp` or `upgrade-tenant` — so existing tenants upgraded
  to a seed-capable binary are never seeded.

### Additive refresh (Phase 4)

- **Published source generation** — mt-admin invalidates the generation marker,
  fast-forwards a clean root-owned checkout under `flock`, validates its Study
  Notes files, and atomically publishes
  `.git/lunarwing-study-notes-generation.json`. A host-global systemd timer or
  OpenRC cron entry updates every unique checkout used by an enabled tenant
  hourly. A tenant pass reads the marker before and after its scan and rejects a
  pass spanning an update.
- **Independent daemon cadence** (`knowledge_sync`) — explicitly enabled by
  `KNOWLEDGE_SYNC_ENABLED=true`; it does not depend on heartbeat or chat traffic.
  The default cadence is six hours with deterministic startup jitter, a
  cross-process single-flight lock, durable revision/failure state, and bounded
  exponential failure backoff. Each pass imports at most 500 missing notes and
  10 MiB by default. Existing paths are never changed or deleted.
- `add-tenant --with-study-mcp` is the opt-in path for new tenants and enables
  both the one-shot bootstrap and additive refresh. Existing tenants opt in with
  `configure-study-mcp <tenant> --enable-knowledge-sync`, then restart once.
  `refresh-knowledge <tenant> [--sync-source]` runs the same bounded importer
  without a daemon restart; `sync-knowledge-sources` is the timer entrypoint.
- This 2.1 fallback reports `mode=additive` and a Git source revision. It closes
  the growing missing-note gap but does not claim hosted-store parity and does
  not propagate upstream edits, lifecycle changes, or tombstones. A revisioned
  hosted snapshot/delta contract remains the prerequisite for those operations.

libSQL backend: implemented with FTS5 (`memory_chunks_fts`) and per-scope
merge; note that FTS5 rank values are not comparable across scopes, so merged
results carry descending ordinal ranks.

## Hybrid Search (RRF)

For a deep dive into the full semantic search pipeline (embedding providers, chunking, fusion strategies, database schema, and configuration), see `docs/architecture/SEMANTIC-MEMORY-SEARCH.md`.

Combines full-text search and vector similarity using Reciprocal Rank Fusion:

```
score(d) = Σ 1/(k + rank(d)) for each method where d appears
```

Default k=60. Results from both methods are combined, with documents appearing in both getting boosted scores.

**Backend differences:**
- **PostgreSQL:** `ts_rank_cd` for FTS, pgvector cosine distance for vectors, full RRF
- **libSQL:** FTS5 for keyword search + vector search via `libsql_vector_idx` (dimension set dynamically by `ensure_vector_index()` during startup)

## Multi-Scope Reads & Identity Isolation

When a workspace has additional read scopes (via `with_additional_read_scopes`), read operations can span multiple user scopes — a user with scopes `["alice", "shared"]` can read documents from both.

**Identity files are exempt from multi-scope reads.** The system prompt reads identity and configuration files from the **primary scope only** (`read_primary()`), never from secondary scopes:

| File | Read method | Rationale |
|------|------------|-----------|
| AGENTS.md | `read_primary()` | Agent instructions are per-user |
| SOUL.md | `read_primary()` | Core values are per-user |
| USER.md | `read_primary()` | User context is per-user |
| IDENTITY.md | `read_primary()` | Identity is per-user |
| TOOLS.md | `read_primary()` | Tool config is per-user |
| BOOTSTRAP.md | `read_primary()` | Onboarding is per-user |
| lorebooks/*.json | `list_primary()`/`read_primary()` | Prompt-visible world lore is per-user |
| MEMORY.md | `read()` | Shared memory is a feature |
| daily/*.md | `read()` | Shared daily logs are a feature |

**Why:** Without this, a user with read access to another scope could silently inherit that scope's identity if their own copy is missing. The agent would present itself as the wrong user — a correctness and security issue.

**Design rule:** If you want shared identity across users, seed the same content into each user's scope at setup time. Don't rely on multi-scope fallback for identity files.

### Lorebooks

`lorebooks/*.json` accepts the bounded SillyTavern World Info import subset and
native list shape documented in `docs/proposals/LOREBOOKS.md`. Create or update
a lorebook using `memory_write` with `append: false`; append is rejected before
storage mutation. Lorebooks are disabled by default because activation scanning
is computationally expensive; set `LOREBOOK_ENABLED=true` to opt in.

Lorebooks are prompt-visible configuration. Raw JSON and decoded fields receive
canonicalized prompt-injection checks on default writes, named-layer writes,
and legacy loads. Only primary-scope lorebooks load. Cache validation uses a
bounded revision query; cache misses fetch size-capped content in one
path-ordered query. Parsed entries are cached by primary scope, agent, and
document revision; writes and deletes invalidate immediately.
Empty listings receive a short negative cache, while revision checks detect
legacy/direct-database changes. Group-chat activation
defaults off. Database listing, file, entry, key, scan, token, operation, and
rendered-output limits bound per-turn work; see the proposal for exact values
and unsupported SillyTavern behavior.

## Heartbeat System

Proactive periodic execution (default: 30 minutes):

1. Reads `HEARTBEAT.md` checklist
2. Runs agent turn with checklist prompt
3. If findings, notifies via channel
4. If nothing, agent replies "HEARTBEAT_OK" (no notification)

```rust
use crate::agent::{HeartbeatConfig, spawn_heartbeat};

let config = HeartbeatConfig::default()
    .with_interval(Duration::from_secs(60 * 30))
    .with_notify("user_123", "xmpp");

spawn_heartbeat(config, workspace, llm, response_tx);
```

## Chunking Strategy

Documents are chunked for search indexing:
- Default: 800 words per chunk (roughly 800 tokens for English)
- 15% overlap between chunks for context preservation
- Minimum chunk size: 50 words (tiny trailing chunks merge with previous)

Internal Engine state under `engine/.runtime/**` is stored as normal workspace
documents for restart recovery but receives no chunks or embeddings. The same
holds for the mission-directory subtree `engine/projects/*/missions/*/**` and
the project-level `engine/projects/*/missions.json` manifest (WI#135):
everything under a mission directory — not just the exact `mission.json`
basename — stays readable via `read()`/`list()` (mission hydration reads
documents, not chunks) but never enters search. Legacy chunks created by
pre-fix builds are removed by the idempotent
`purge_mission_index_chunks()` sweep that runs at boot (before the embedding
backfill) and on the heartbeat hygiene pass.
