# Review

## Review: `aj/kimi-14-20260816-0002` vs `origin/master`

## Summary

This branch is a 16-commit feature series introducing cross-tenant knowledge sharing through a new coordination record:

```text
KNOW: <topic-slug> | <one-line finding>
```

The changes add parsing and ingestion, daemon registration, coalescing behavior, multi-tenant environment propagation, documentation, and tests. The implementation includes several sound safeguards, but it also introduces compatibility, trust-boundary, consistency, and resource-exhaustion risks.

**Recommendation:** Do not merge until findings 1 through 3 are resolved. Findings 4 and 5 should be addressed before enabling the feature in shared production rooms.

## Findings

### 1. High: Rolling-fleet compatibility does not cover older binaries

`ic/src/hooks/coordination.rs:39` adds `KNOW` to the default coordination schema. `docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md:206` states that this keeps rolling and mixed-opt-in fleets from striking senders.

That protection applies only to upgraded daemons. A daemon still running `origin/master` recognizes only the original five tags. It will reject an inbound `KNOW:` message as a schema violation and can contribute to the sender's three-strike disablement.

This means `KNOW` cannot safely be emitted during a genuine mixed-version rollout.

**Recommended fix:** Do not enable emission until all room participants support `KNOW`, or introduce explicit protocol capability/version negotiation.

### 2. Medium: Any room occupant can inject content into tenants' default knowledge context

`ic/src/hooks/coordination_knowledge.rs:375` accepts any non-empty, bounded ASCII `sender_id`. It does not require the sender to appear in `COORD_ADMIN_USERS`, `COORD_RESPONDER_ROSTER`, or a dedicated publisher allowlist.

Accepted content is written verbatim into the shared knowledge scope at `ic/src/hooks/coordination_knowledge.rs:298`. Knowledge tools search private and shared scopes by default at `ic/src/tools/builtin/knowledge.rs:118`, and include every lifecycle status unless callers explicitly filter at `ic/src/tools/builtin/knowledge.rs:293`.

The tools then return this content without sanitization:

- `knowledge_search`: `ic/src/tools/builtin/knowledge.rs:331`
- `knowledge_get_topic`: `ic/src/tools/builtin/knowledge.rs:422`

The existing safeguards reduce but do not eliminate the risk:

- Records are marked `needs-verification`.
- Messages must be one line and normally no more than 420 characters.
- Provenance and the `coordination` tag are retained.
- Secret detection runs before ingestion.

A compromised or unexpected room participant can still plant instruction-like text that later enters another tenant's model context. The ownership check proves consistency with the claimed sender; it does not authenticate that sender.

**Recommended fix:** Add an explicit knowledge-publisher allowlist, preferably bound to authenticated channel identities. At minimum, document the trusted-room-membership requirement and sanitize content returned by the knowledge tools.

### 3. Medium: Timeout cancellation can permanently wedge a sender/topic path

The ingestion path performs two separate mutations:

1. Write content and search chunks at `ic/src/hooks/coordination_knowledge.rs:318`.
2. Attach study-note metadata at `ic/src/hooks/coordination_knowledge.rs:322`.

Hooks use a five-second default timeout at `ic/src/hooks/hook.rs:253`. The registry drops the hook future when that deadline expires at `ic/src/hooks/registry.rs:103`.

If cancellation occurs after the content transaction commits but before metadata is attached, `restore_previous` never runs because rollback only handles an explicit metadata error. The resulting document has missing or stale metadata.

Knowledge-specific queries will not return a new document without the `study-note` provenance marker, but future shares for the same sender and topic fail the ownership check at `ic/src/hooks/coordination_knowledge.rs:284`. The path remains unusable until an operator manually repairs or deletes it.

The global ingestion mutex increases the likelihood of queued shares reaching the timeout during slow database operations.

**Recommended fix:** Commit content and metadata in one database transaction. If that is not immediately possible, add a cancellation-safe repair path for incomplete wire notes and review the hook timeout.

### 4. Medium: Sender-controlled limiter keys permit unbounded memory and note growth

The ingestion limiter key contains the room-controlled sender identity at `ic/src/hooks/coordination_knowledge.rs:381`.

`CoordinationState` stores limiter windows in a `HashMap<String, Vec<Instant>>`. At `ic/src/hooks/coordination.rs:422`, timestamps are pruned only for the selected key; empty or expired keys are never removed from the map.

Before this branch, limiter keys were effectively bounded by configured agent/channel pairs. A participant can now rotate sender identities to:

- Create process-lifetime map entries.
- Obtain a fresh per-sender ingestion allowance.
- Create distinct persistent note paths for each sender and topic.
- Queue additional work behind the global ingestion mutex.

The in-memory map resets on restart and each key is bounded in size, but the number of keys is not bounded.

**Recommended fix:** Add whole-map expiry and a maximum key count, enforce a global ingestion budget, and bind limiter identity to authenticated publishers.

### 5. Medium: Mission formatting corrupts machine-readable `KNOW` records

`format_mission_notification` uses `DEFAULT_ALLOWED_TAGS` at `ic/src/bridge/router.rs:3479`. Because this branch adds `KNOW` to that global constant, a mission result such as:

```text
KNOW: topic | finding
```

is transformed into content similar to:

```text
KNOW: **[mission]** topic | finding
```

The receiver interprets `**[mission]** topic` as the topic slug and rejects it. The added Markdown also violates the coordination protocol's machine-readable, no-Markdown requirement.

**Recommended fix:** Do not decorate `KNOW` records, or give machine-readable tags a separate formatting path rather than deriving mission behavior from the global schema tag list.

### 6. Low: PostgreSQL and failure-path coverage is missing

All knowledge-ingestion integration tests are gated on libSQL at `ic/src/hooks/coordination_knowledge.rs:485`. Provisioned multi-tenant deployments use PostgreSQL, so the primary production path has no direct coverage of the content, chunk, and JSONB metadata interactions.

Important untested behavior includes:

- PostgreSQL write and metadata round trips.
- Ownership-mismatch refusal.
- Exact duplicate no-op behavior.
- Metadata failure and cancellation recovery.
- Configured message-cap enforcement.
- Invalid environment limit handling.
- Secret detection running before ingestion.
- Successful six-hook registration and priority ordering.
- End-to-end ingestion followed by `knowledge_search`.

This is primarily a coverage gap, but it affects the highest-risk invariants in the feature.

## Secondary Observations

- Invalid `COORD_KNOWLEDGE_MAX_PER_HOUR` and `COORD_KNOWLEDGE_MIN_CHARS` values silently fall back to defaults without warning.
- Rate-limit quota is consumed before duplicate, ownership, and preflight failure checks.
- Self-echo suppression is reliable in the native XMPP path, but other adapters or reconnect races may produce redundant `coord-<self>` notes.
- Unrelated editorial changes to goal items 13 and 16 add avoidable documentation churn.

## Verified Behavior

The following parts were reviewed and appear sound:

- Unicode-safe tag parsing and UTF-8 truncation.
- Single-line and control-character enforcement.
- Exact coordination-room matching.
- Secret detection runs before knowledge ingestion.
- `coord-` namespace prevents ordinary native-note path collisions.
- The 96-bit sender hash distinguishes normalized sender collisions.
- Missing sender provenance does not fall back to the tenant owner.
- `KNOW:` bypasses outbound coalescing without changing `ALERT:` behavior.
- Multi-tenant environment defaults, preservation, and backfill are init-agnostic.
- No bare tenant `systemctl` or `rc-service` logic was introduced.

## Verification Results

Reviewed 16 branch commits over merge base `41f15de6`, covering 11 changed files and 1,031 insertions.

The following checks passed:

- `cargo fmt --all -- --check`
- `bash -n` for the modified shell scripts
- `scripts/tests/test-add-tenant-env-flags.sh`
- Coordination-filtered test suite: 59 passed
- Focused knowledge-hook suite: 9 passed
- `git diff --check`

The worktree remained clean.

---

## Resolution (2026-08-17, aj/kimi-14-20260817-2100)

All findings were addressed on `aj/kimi-14-20260817-1800` and verified after
merge into `aj/kimi-14-20260817-2100`:

1. **Rolling-fleet compatibility** — `KNOW` is schema-valid on every upgraded
   daemon (`DEFAULT_ALLOWED_TAGS`), while emission is gated behind
   `COORD_KNOWLEDGE_PUBLISH` (default `false`). Until opted in, the per-turn
   prompt never teaches the model the wire format, `TagSchemaHook` rejects
   outbound `KNOW:` at `BeforeOutbound` without striking, the `message` tool
   refuses direct sends (`ToolError::NotAuthorized`), and `ChannelManager`
   enforces the same gate at the respond/broadcast/broadcast_all delivery
   boundary. Documented operator preconditions in
   `docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md`.
2. **Unauthenticated room injection** — ingestion requires the
   `COORD_KNOWLEDGE_PUBLISHERS` sender allowlist (default empty = deny all),
   matched case-insensitively against the full inbound sender ID. Knowledge
   tools mark coordination notes `untrusted: true`, replace the
   sender-controlled title with a topic-derived label, wrap snippets/content
   in `lunarwing_safety::wrap_external_content`, and both content-reading
   tools now declare `requires_sanitization() == true`.
3. **Timeout cancellation wedge** — ingestion runs on a detached task the
   registry hook deadline cannot cancel between the content and metadata
   commits, serialized by a daemon-wide ingest mutex. A crash between commits
   is recognized by the preflight (`incomplete_wire_write`) and repaired by
   the same sender/topic's next share; outright metadata failure restores the
   previous document (or deletes the partial new one).
4. **Sender-controlled limiter keys** — `try_consume_rate_limit` prunes
   expired windows on every call and caps retained buckets at exactly
   `MAX_RATE_LIMIT_KEYS` (4096) including a reserved overflow bucket; unseen
   keys past the cap share one budget-sized bucket. Knowledge ingestion adds
   an independent daemon-wide window (`COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR`,
   default 300, max 10000), and knowledge limiter keys derive from the
   operator allowlist.
5. **Mission formatting corruption** — `format_mission_notification` returns
   `KNOW:` records verbatim and derives decoration from the presentation-safe
   `MISSION_DECORATION_TAGS` subset instead of the wire schema constant.
6. **Coverage gaps** — PostgreSQL integration test
   (`ic/tests/coordination_knowledge_postgres.rs`, gated on `DATABASE_URL`)
   exercises ingest, JSONB metadata, full-text search, topic lookup, and
   cleanup through the production `Database` path. libSQL integration tests
   cover ownership refusal, duplicate no-op, stranded-write repair, message
   cap, secret-before-ingest ordering, sender/global rate limits, and
   six-hook registration order. Env-limit parsing tests (invalid/out-of-range
   fallback, range boundaries, publisher validation/dedup) were added on this
   branch.

Secondary observations: invalid env values now log a warning and fall back to
defaults; quota-before-duplicate ordering is documented as deliberate
(flood protection); redundant self-echo notes are absorbed by the exact
duplicate no-op; the unrelated goal-item 13/16 edits are not present in the
merged branch.

Additional fix on this branch: `write_tenant_lunarwing_env` in
`ic/scripts/lunarwing-mt-admin.sh` now declares `coord_knowledge_publish` and
`coord_knowledge_publishers` as `local` (they were assigned undeclared).

### Resolution verification

- `cargo fmt --all -- --check` clean
- `cargo clippy --lib --tests -- -D warnings` exit 0
- Targeted suites: coordination 59, coordination_knowledge 20 (incl. 3 new
  env tests), channels::manager 11, channels::coalesce 25,
  tools::builtin::message 26, tools::builtin::knowledge 10, bridge::router 61
- `bash -n ic/scripts/lunarwing-mt-admin.sh` and
  `ic/scripts/tests/test-add-tenant-env-flags.sh` pass
