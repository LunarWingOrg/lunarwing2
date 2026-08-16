# Review

I’ll establish the branch’s actual merge base and change scope first, then review the affected subsystems against their local specifications and targeted tests. I’ll prioritize behavioral defects, security or compatibility regressions, and gaps in validation.

The branch is a 16-commit feature series over `origin/master`, centered on a new `KNOW` coordination record and ingestion hook, with daemon wiring and multi-tenant configuration propagation. I found no existing study note for this feature, so I’m verifying the implementation directly and splitting the review across the parser/security boundary, runtime integration, and deployment behavior.

The storage path uses ordinary workspace writes plus a separate

One concrete integrity concern is emerging: ingestion is wrapped by the generic five-second hook timeout, but the note update

Review complete. I verified

 the shell test passes locally (ALL TESTS PASSED), CI wiring exists (`.gitlab/scripts/shell-harnesses.sh` globs `ic/scripts/tests/*.sh`), bash syntax is clean, and cross-checked docs against code constants. Findings ordered by severity:

## Medium

1. **Knowledge-share DB integration tests are libsql-only, but mt-admin tenants are postgres-only** — `ic/src/hooks/

## Review: `ic/src/hooks/coordination_knowledge.rs` (branch `aj/kimi-14-20260816-0002` vs `origin/master`)

I read the new hook, its registration path (`coordination.rs`, `main.rs`, `mod.rs`), the coalescer change, the workspace/DB layers it touches (`workspace/mod.rs`, `workspace/knowledge.rs`, `db/libsql/workspace.rs`), hook registry execution semantics, the knowledge sweep, and the updated specs (`DEPLOYMENT.md`, `.env.example`, mt-admin). Findings, ordered by severity:

### HIGH

**H1 — Unauthenticated sender identity: any room participant can poison every opted-in tenant's knowledge base (design-level trust gap).**
- `execute` (coordination_knowledge.rs:375-395) accepts any `sender_id` that is merely non-empty, ≤128 ASCII-graphic bytes. There is no allowlist (admin roster, responder roster, tenant registry) and no authentication binding between `sender_id` and a real tenant. Room nicks/JIDs are spoofable.
- Ingested content becomes searchable agent context via `knowledge_search` (shared herd scope is in the default read scopes, src/tools/builtin/knowledge.rs:118-134). This is an indirect prompt-injection path into every opted-in tenant's agents: a single compromised/malicious room member writes attacker-controlled prose that all tenant LLMs later consume. Mitigations exist (`needs-verification`, `coordination` tag, `submitted_by` provenance, lines 298-310) but nothing gates *who* may publish.
- Related: the ownership check `wire_note_owned_by` (207-222) only proves consistency with the *claimed* sender, so a spoofer impersonating sender `a@b` can freely overwrite `a@b`'s wire notes (`submitted_by` case-insensitive compare at 220 makes this easier, not harder). The DEPLOYMENT.md wording ("metadata does not prove the same wire sender") overstates the protection — it cannot distinguish the true origin.
- Recommendation: optional sender/tenant allowlist env (`COORD_KNOWLEDGE_ALLOWED_SENDERS`), and surface `coordination`-tag/`submitted_by` in search results prominently so agents are instructed to distrust wire notes. At minimum, document the room-trust threat model explicitly.

### MEDIUM

**M1 — Cancellation/timeout between `write` and `set_document_metadata` permanently wedges a (sender, topic) path.**
- New documents are created with metadata `'{}'` (src/db/libsql/workspace.rs:351-352). `ingest` writes content first (318-321), then attaches metadata (322-330). The rollback path (`restore_previous`, 257-276) only fires on a metadata *error* — not on future cancellation.
- Hooks run under `tokio::time::timeout(hook.timeout(), …)` (src/hooks/registry.rs:106) with the default 5s timeout (src/hooks/hook.rs:256-258); `KnowledgeShareHook` does not override it. One ingest

A second trust-boundary issue is confirmed: `knowledge_search` includes every lifecycle status by default, so these room-authored `needs-verification` records are returned to the model alongside verified notes unless callers explicitly filter. The status

**L4 — Default 5s hook timeout not reviewed/documented for this DB-bound hook** — see M1; also each coordination inbound message's hook chain now includes up to ~6 DB round trips (latency added before tag-schema/engagement

# Review: `aj/kimi-14-20260816-0002` vs `origin/master` — coordination knowledge sharing

Scope reviewed: `ic/src/hooks/coordination_knowledge.rs` (new), `ic/src/hooks/coordination.rs`, `ic/src/hooks/mod.rs`, `ic/src/channels/coalesce

---

## Findings (severity ordered)

### 1. MEDIUM-HIGH (security, design) — Any room occupant can inject content into every opted-in tenant's agent context; no sender trust gate
`ic/src/hooks/coordination_knowledge.rs:280-332, 349-397`. The hook ingests `KNOW:` records from **any** sender with no allowlist: `COORD_ADMIN_USERS` and `COORD_RESPONDER_ROSTER` are never consulted. `allow_rooms` gates the room, not occupants. Ingested notes land in the `shared` scope, which `knowledge_search`/`knowledge_get_topic` read **by default** (`src/tools/builtin/knowledge.rs:120-132` — `read_scopes("all")` = `[user, "shared"]`), so attacker-controlled one-liners (≤420 chars)

 `coordination_knowledge.rs:381-385`. `try_consume_rate_limit` prunes stale

## Validated findings — branch `aj/kimi-14-20260816-0002` vs `origin/master`

### 1. Untrusted `KNOW:` sender content returned unsanitized by knowledge tools — VALID (Medium severity)
- Ingest accepts any wire sender (non-empty, ≤128-byte ASCII-graphic; no allowlist/auth): `ic/src/hooks/coordination_knowledge.rs:161-167, 375-378`; content stored verbatim as `needs-verification` in the shared herd scope: `coordination_knowledge.rs:298-311, 318-325`.
- `knowledge_search`/`knowledge_get_topic` read shared scope by default (`ic/src/tools/builtin/knowledge.rs:119-137, 300, 388`) and return wire text raw (800-byte snippet `knowledge.rs:314-315`; 3000-byte content `404-407`) while declaring `requires_sanitization() == false` under the now-false rationale "Internal knowledge store, trusted content": `knowledge.rs:331-333, 422-424`.
- Injection scanning does **not** apply: `validate_prompt_write` only scans system-prompt/lorebook paths (`ic/src/workspace/mod.rs:243-246`), so `study-notes/` writes bypass it.
- Mitigations limiting severity: single-line, no control chars, ≤420 chars default (`coordination_knowledge.rs:127-146`); SecretEntropyHook (priority 5, Reject stops chain, `registry.rs:109-112`) blocks secret-shaped payloads before the priority-13 knowledge hook; status/provenance labels are surfaced. Net: cross-tenant prompt-injection surface into the local agent's tool context — real, bounded, Medium.

### 2. Default 5s hook timeout can cancel between `write` and `set_document_metadata` — VALID (Low-Medium)
- Default timeout 5s (`ic/src/hooks/hook.rs:256-257`); `KnowledgeShareHook` does not override `timeout()`. Registry drops the future at the deadline and FailOpen just logs (`ic/src/hooks/registry.rs:104-106, 133-139`) — the `restore_previous` rollback (`coordination_knowledge.rs:322-330`) only fires on an `Err`, never on cancellation.
- `write` (content+chunks+embeddings, one txn, `mod.rs:915-934`) and `set_document_metadata` (`mod.rs:1928-1940`) are separate transactions; the write path includes embedding generation, making >5s plausible.
- Consequence: orphan doc with default metadata. Caveats that temper severity: knowledge queries filter on `metadata->>'provenance' = 'study-note'` (`ic/src/workspace/repository.rs:1002, 1061`), so the partial note is **not** returned by search; the real damage is the wedge — next same-sender share hits the ownership check `wire_note_owned_by` (`coordination_knowledge.rs:284-297`) with unparseable metadata → permanent "ownership mismatch", that sender+topic can never be re-ingested without manual DB cleanup.

### 3. Sender-controlled limiter keys → unbounded rate map — VALID (Low-Medium DoS)
- Key embeds raw sender: `\0knowledge-share\0{sender-lowercased}` (`coordination_knowledge.rs:381`); storage is `HashMap<String, Vec<Instant>>` (`ic/src/hooks/coordination.rs:365-371`). `try_consume_rate_limit` prunes only timestamps within each entry, never removes empty entries (`coordination.rs:424-439`) — entries persist for process lifetime. (Contrast: `responder_claims` IS TTL-pruned, `coordination.rs:451-453`.)
- Any room participant can present unbounded distinct sender IDs (each ≤128 bytes) with valid `KNOW:` payloads; each also independently earns `max_per_hour` ingests, so note storage in the shared scope grows per-sender too.
- Caveats: requires opt-in (`COORD_KNOWLEDGE_SHARE=true` + configured room); keys are bounded ~150 bytes each; in-memory only, reset on restart. Versus `RateLimitHook`'s config-bounded key (`coordination.rs:1067`), this is the genuinely unbounded axis.

### 4. Integration tests are libsql-only, postgres path untested — VALID but Low (matches repo convention)
- `#[cfg(feature = "libsql")] mod integration` at `coordination_knowledge.rs:485` uses `crate::testing::test_db`, which is libsql-only (`ic/src/testing/mod.rs:49-56`). No postgres test for the hook despite postgres being primary (default features include both, `Cargo.toml:243`).
- Caveats: the hook only exercises backend-agnostic workspace APIs (`read_primary`/`write`/`set_document_metadata`/`delete`); sibling knowledge modules have the same libsql-only pattern (`knowledge_sweep.rs:274`, `workspace/mod.rs:3031`), and a postgres harness pattern exists (`tests/workspace_integration.rs:1-5`, `#![cfg(feature = "postgres")]`) but isn't used here. Coverage gap, not a behavioral defect.

**Rejected/adjusted:** none of the four are speculative, but claims of "poisoned search results" from partial writes (finding 2) and "unbounded storage" in the sense of persistence (finding 3's map is RAM-only) should be worded as above.

**Findings**

1. **High: The claimed rolling-fleet compatibility does not cover older binaries.**  
[coordination.rs](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination.rs:39) adds `KNOW` to the default schema, and [DEPLOYMENT.md](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md:206) says this keeps rolling or mixed fleets from striking senders. It only protects upgraded daemons with ingestion disabled. A daemon still running `origin/master` recognizes five tags, rejects inbound `KNOW:`, records a schema violation, and can contribute to the sender’s three-strike disablement. During an actual rolling deployment, `KNOW` must not be emitted until all watchers support the tag, or protocol capability/version negotiation is needed.

2. **Medium: Any coordination-room occupant can publish into every opted-in tenant’s default knowledge context.**  
[coordination_knowledge.rs](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination_knowledge.rs:375) validates only that `sender_id` is bounded ASCII; it does not require membership in an admin, responder, or knowledge-publisher allowlist. Accepted text is written verbatim to shared scope at [line 298](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination_knowledge.rs:298). The knowledge tools search private plus shared scope by default at [knowledge.rs](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/tools/builtin/knowledge.rs:118), including all statuses when no filter is supplied at [line 293](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/tools/builtin/knowledge.rs:293). `needs-verification`, one-line bounds, and provenance reduce the impact, but they do not prevent a compromised or unexpected room member from planting instruction-like text that later enters model context. Add an explicit publisher allowlist or define and enforce a trusted-room-membership contract.

3. **Medium: Hook timeout cancellation can permanently wedge a sender/topic path.**  
The hook writes content at [coordination_knowledge.rs:318](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination_knowledge.rs:318), then attaches metadata in a separate operation at [line 322](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination_knowledge.rs:322). Hooks have a five-second default timeout at [hook.rs](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/hook.rs:253), and the registry drops the future when that expires at [registry.rs](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/registry.rs:103). Cancellation after the content transaction commits skips `restore_previous`, leaving metadata empty or stale. Future shares then fail the ownership check at [coordination_knowledge.rs:284](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination_knowledge.rs:284), requiring manual cleanup. Content and metadata should be committed atomically, or incomplete wire-owned records need a safe repair path.

4. **Medium: Sender-controlled limiter keys introduce unbounded process-memory and storage growth.**  
The new key includes the room-controlled sender at [coordination_knowledge.rs:381](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination_knowledge.rs:381). `try_consume_rate_limit` retains timestamps only within the selected key and never evicts empty map entries at [coordination.rs](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination.rs:422). Before this branch, the key space was effectively bounded by configured agent/channel pairs. Rotating sender identities now creates permanent map entries and independently resets the per-sender ingestion allowance, permitting unbounded distinct note paths. Add global/key-count limits and periodic whole-map eviction; ideally bind limiter identity to authenticated publishers.

5. **Medium: Expanding the global tag constant corrupts mission-produced `KNOW` records.**  
`format_mission_notification` uses `DEFAULT_ALLOWED_TAGS` at [router.rs](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/bridge/router.rs:3479). Because this branch adds `KNOW` to that global constant, a mission result such as `KNOW: topic | finding` becomes `KNOW: **[mission]** topic | finding`. Receivers parse `**[mission]** topic` as the topic, reject it as an invalid slug, and do not ingest the record. It also violates the coordination protocol’s no-markdown requirement. `KNOW` needs dedicated formatting, or mission decoration must be skipped for machine-readable records.

6. **Low: Primary-backend and failure-path coverage is missing.**  
All five database integration tests are gated on `libsql` at [coordination_knowledge.rs:485](/var/lib/paseo/.paseo/worktrees/1ymbgy9w/quaint-eagle/ic/src/hooks/coordination_knowledge.rs:485), while provisioned multi-tenant deployments use PostgreSQL. There are also no tests for ownership refusal, exact duplicate no-op, metadata rollback/cancellation, configured message caps, invalid environment limits, or secret-before-ingest registry ordering. The PostgreSQL gap is especially relevant because the feature relies on content, chunk, and JSONB metadata behavior across separate operations.

**Verification**

Reviewed 16 branch commits over merge base `41f15de6`, covering 11 changed files and 1,031 insertions. `cargo fmt --all -- --check`, shell syntax, and `scripts/tests/test-add-tenant-env-flags.sh` passed. The coordination-filtered suite passed 59 tests, and the focused knowledge-hook suite passed 9 tests. `git diff --check` passed and the worktree remains clean.

I would not merge this branch until findings 1 through 3 are resolved. Findings 4 and 5 should also be fixed before enabling the feature on shared production rooms.