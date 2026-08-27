# Item 2 — Cross-Tenant Knowledge Sharing: Concern Resolutions + E2E Verification

**Date:** 2026-08-20 · **Branch:** `aj/kimi-2-20260820-0301` · **Host:** Arch Linux (systemd, user-manager tenants)

Addresses `docs/ops/history/v2.0.8.0/GOAL2_INPUT_CONCERNS.md` (ruffles' review of
the `KNOW:` cross-tenant knowledge pipeline, `ic/src/hooks/coordination_knowledge.rs`),
then re-verifies live cross-tenant sharing with three throwaway tenants.

## Concern resolutions

### 1. Global `ingest_lock` serializes ALL KNOW writes — FIXED

The single daemon-wide `Mutex<()>` is replaced by 64 per-path striped locks
(`ingest_locks: Arc<Vec<Mutex<()>>>`, FNV-1a stripe selection on the note path).
The invariant the lock protects — a note's content commit and metadata commit
must not interleave with another write to the *same* path — only ever required
same-path serialization. Different sender/topic paths now ingest fully
concurrently; a busy room no longer queues every write behind one mutex.

### 2. Global rate limit approximate, not a hard guarantee — ALREADY HARD; REGRESSION-PROVEN

The premise was stale for the reviewed code: `try_consume_global_limit` (and the
per-sender `CoordinationState::try_consume_rate_limit`) performs
retain + check + insert under a single lock acquisition, so two concurrent hook
executions cannot both pass — admission is atomic. Added regression test
`global_limit_admission_is_atomic_under_concurrency`: 50 racing admissions
against a budget of 5 admit exactly 5. One documented semantic caveat: the
budget counts *admissions* (accepted ingest work), not completed writes — a
write that later fails still consumed its slot. That is the correct
fail-closed behavior for a flood guard.

### 3. Stranded writes only self-heal if the sender shares again — FIXED

The periodic knowledge sweep (`ic/src/workspace/knowledge_sweep.rs`, runs from
the heartbeat on its normal cadence, default 24h) now reaps stranded wire notes
in the shared scope: paths under `study-notes/coord-*` carrying the wire
sentinel, without study-note metadata, older than a 24h grace period. The grace
window guarantees an in-flight or freshly stranded ingest is never touched.
`KnowledgeSweepReport.stranded_removed` is surfaced in the heartbeat log.
Previously the same-sender repair path was the only cleanup.

### 4. Prompt injection via knowledge tools is mitigated, not eliminated — VERIFIED COMPLETE, RESIDUAL INHERENT

Audit of every read path for wire notes:

- `knowledge_search` snippets and `knowledge_get_topic` full content pass
  through `protect_note_content`: coordination-tagged notes are truncated, then
  wrapped in the standard `wrap_external_content` SECURITY NOTICE
  ("EXTERNAL, UNTRUSTED"), and their metadata gains `untrusted: true` with the
  sender-controlled title replaced by a topic-derived label. Both tools declare
  `requires_sanitization() == true`, and both tool descriptions instruct the
  model to treat such results as data.
- `knowledge_list_stale` rows carry `untrusted: true` and the rewritten title;
  no content is returned.
- `knowledge_stats` returns counts only (now including a `coordination` count
  so operators can see how much untrusted material the store holds).

Improvement landed under this item: the `include_coordination=false` escape
hatch (concern 6) also shrinks routine exposure to injected content. The
remaining residual — a determined injector's text still enters model context
inside the warning wrapper, and a routine reading the raw workspace document
directly bypasses the knowledge-tool wrapping — is inherent to any shared
knowledge store and is now documented here and in the deployment doc. No
further code mitigation is possible without removing the feature's value.

### 5. `incomplete_wire_write` heuristic (string prefix match) — FIXED

Wire note bodies now begin with a machine-checkable sentinel line,
`<!-- lunarwing:coordination-wire-note -->`, written from the first content
commit (`render_wire_note_markdown`). Detection
(`workspace::is_incomplete_wire_write`) is an exact marker match, not a title
heuristic. Notes stranded before the sentinel existed are still recognized via
the legacy `# KNOW: ` prefix check, so mixed-version stores keep repairing.
The sentinel is an HTML comment: invisible in rendered Markdown, inert to FTS
consumers, and inside the untrusted-content wrapper on reads.

### 6. Routine sweeps encounter coordination notes — FIXED (opt-out guard + visibility)

- `knowledge_list_stale` gained `include_coordination` (default `true`, so
  existing behavior is preserved). With `false`, coordination-tagged wire notes
  are excluded before stale evaluation, so routines that auto-process
  `needs-verification` notes never act on unverified cross-tenant claims. The
  response echoes the flag under `criteria` for auditability, and rows were
  already flagged `untrusted: true` for routines that prefer to filter
  client-side.
- `knowledge_stats` now reports a `coordination` count (untrusted wire-note
  volume per scan) alongside the lifecycle counts.
- Tool descriptions for both tools document the guidance.

## Code changes

| File | Change |
|------|--------|
| `ic/src/workspace/knowledge.rs` | `WIRE_NOTE_SENTINEL`, `WIRE_AGENT_PREFIX`, `render_wire_note_markdown`, `is_incomplete_wire_write`; `KnowledgeStats.coordination` count; tests |
| `ic/src/workspace/mod.rs` | re-export the new knowledge items |
| `ic/src/hooks/coordination_knowledge.rs` | 64-stripe per-path ingest locks replace the global mutex; sentinel bodies; shared stranded detection; concurrency regression test |
| `ic/src/workspace/knowledge_sweep.rs` | stranded-wire-note reaper in the shared-scope sweep (24h grace), `stranded_removed` report field, tests |
| `ic/src/agent/heartbeat.rs` | heartbeat sweep log surfaces `stranded_removed` |
| `ic/src/tools/builtin/knowledge.rs` | `include_coordination` param on `knowledge_list_stale`, `coordination` count surfaced by `knowledge_stats`, tests |
| `docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md` | striped locks, sentinel, sweep reaping, hard rate-limit guarantee, routine opt-out, stats count |

## Verification (unit/integration)

- `cargo test --lib knowledge` (libsql-backed): **68 passed, 0 failed** —
  includes new `global_limit_admission_is_atomic_under_concurrency`,
  `sweep_reaps_only_grace_expired_stranded_wire_notes`,
  `include_coordination_param_defaults_true`,
  `stale_selection_can_exclude_coordination_notes`,
  `wire_note_markdown_carries_the_sentinel`,
  `incomplete_wire_write_detection`, updated `stats_aggregation`, plus the full
  pre-existing KNOW hook and sweep suites.
- Regression nets: `cargo test --lib workspace::` **178/178**, `cargo test --lib hooks::` **121/121**.
- `cargo check --all-features` and `cargo check --all-features --tests`: clean.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --lib --tests -- -D warnings`: clean.

## Live E2E (3 test tenants)

Procedure mirrors the item-14 run (`docs/ops/ITEM14_CROSS_TENANT_KNOWLEDGE_E2E.md`,
branch `aj/kimi-14-20260819-0601`): local rootless-podman prosody 13.0 on
`127.0.0.1:5222` (VirtualHost `localhost`, MUC `conference.localhost`, room
`coord@conference.localhost`, throwaway test CA in the host trust store for
STARTTLS verification), tenants `sharea`/`shareb`/`sharec` provisioned with
`add-tenant` (this branch's clone — verified HEAD `856465f6` contains all four
fix commits), per-tenant postgres quadlets, `build-tenant --with-wasm`
(release), `start-tenant`. Tenant env deltas (applied before first boot):
`COORD_ENABLED=true`, `COORD_CHANNELS=coord@conference.localhost`,
`COORD_KNOWLEDGE_SHARE=true`, `COORD_KNOWLEDGE_PUBLISH=true`,
`COORD_KNOWLEDGE_PUBLISHERS=coord@conference.localhost/{sharea,shareb,sharec}`,
`COORD_MAX_MESSAGES=50`, `XMPP_ALLOW_ROOMS=coord@conference.localhost`;
sharec additionally got `HEARTBEAT_ENABLED=true`,
`HEARTBEAT_INTERVAL_SECS=180`, `KNOWLEDGE_SWEEP_CADENCE_HOURS=0` for the live
sweep test. All three bridges joined the room and all three daemons logged
`Coordination hooks registered ... knowledge_share=true`.

One bring-up hiccup: at first boot the shareb/sharec bridges logged
`XMPP room join requested` but never acquired room presence
(`rooms_with_presence: []`), while sharea joined fine; a restart of the two
bridge services acquired presence immediately. Initial-join retry robustness
is a pre-existing bridge characteristic, unrelated to the KNOW pipeline
(item 14 did not observe it). Recorded here as an operational note.

### Matrix results

| # | Scenario | Result |
|---|----------|--------|
| T1 | `sharea` sends `KNOW: striped-ingest-locks \| …` | **PASS** — shareb ingested after 20s, sharec after 1s; sharea self-drop (0 rows), by design (MUC self-echo dropped at the XMPP layer) |
| T2 | `intruder@localhost` (not on any allowlist) sends `KNOW: intruder-topic \| …` | **PASS** — 0 matching rows in all three DBs; room traffic itself flowed normally (fail-open) |
| T3 | malformed records from allowlisted sharea (`Not_A_Slug` topic; 5-char finding) | **PASS** — study-note counts unchanged everywhere (sharea 0, shareb 1, sharec 1) |
| T4 | `shareb` sends `KNOW: sweep-stranded-reaper \| …` | **PASS** — sharea + sharec ingested under `coord-…-shareb-4b89a27a…` with `submitted_by=coord@conference.localhost/shareb`; shareb self-drop |
| T5 | sharea re-sends the byte-identical T1 record | **PASS** — `updated_at` bit-identical (`05:16:41.325529+00` before and after); duplicate is a DB no-op |
| T6 | sharea updates its T1 topic with new content | **PASS** — same path replaced, `updated_at` advanced to `05:20:11`, provenance intact, still exactly one row |
| T7 (new) | Sentinel hard marker (concern 5) in live rows | **PASS** — ingested bodies begin `<!-- lunarwing:coordination-wire-note -->` followed by the `# KNOW: …` title in both watcher DBs |
| T8 (new) | Stranded-write sweep live (concern 3) on sharec | **PASS** — seeded `stranded-old.md` (wire namespace, sentinel body, no metadata, `updated_at` backdated 2 days) was reaped at the next heartbeat sweep; the daemon logged `knowledge sweep: reaped stranded coordination wire note path=…` and `stranded_removed=1` in both sweep and heartbeat log lines. The grace-period control (`stranded-fresh.md`, same shape but fresh `updated_at`) survived, as did the legitimate metadata-complete wire note |

Row shape per watcher DB (T1 example, shareb):

```text
path         = study-notes/coord-coord-conference-localhost-sharea-aa7912cd9dd0ec39b495d640/striped-ingest-locks.md
status       = needs-verification
submitted_by = coord@conference.localhost/sharea
tags         = ["coordination"]
confidence / source_commit / studied_at = null
content      = "<!-- lunarwing:coordination-wire-note -->\n\n# KNOW: Per-path striped locks…"
```

## Teardown record

All test infrastructure destroyed on 2026-08-20, immediately after the matrix
completed:

- **Tenants:** `remove-tenant sharea|shareb|sharec --purge` via
  `ic/scripts/lunarwing-mt-admin.sh` — services stopped, ports deallocated, OS
  users and home directories deleted. Verified afterwards: `id share*` → no
  such user; `/home/share*` gone; no `lunarwing-share*`/`xmpp-bridge-share*`
  units; ports registry lists only the pre-existing tenants (`alpha`,
  `hermes`); no tenant containers remain.
- **XMPP infra:** rootless podman container `item2-prosody` stopped and
  removed (`podman ps -a` clean). The prosody accounts died with the
  container's ephemeral storage.
- **Throwaway CA:** `/etc/ca-certificates/trust-source/anchors/item2-test-ca.pem`
  deleted + `update-ca-trust` re-run; `trust list` shows no item2 entry.
- **Secrets:** XMPP passwords and TLS/CA private keys lived only under
  `/tmp/item2-xmpp/` (never in the repo); keys were shredded and the whole
  helper directory plus `/tmp/item2-builds.log` removed.

## Outcome

All six concerns addressed (four code fixes, one verified-already-atomic plus
regression-proofed, one verified-complete with the residual documented);
cross-tenant knowledge sharing re-verified live on the fixed build with three
real tenants (8/8 scenarios pass), including live proof of the new sentinel
marker and the new stranded-write sweep; everything torn down afterwards.
