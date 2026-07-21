## LunarWing v2 Branch Sentinel — 2026-07-21 UTC

### master (d832ad5 → 177fa6c) — unchanged vs last seen
Local master was behind origin by one fast-forward sweep (d832ad5 → 177fa6c), but
no new commits since the previous sentinel run. The FF contained only the
v2.0.1.0 release notes file and the AGENT_GOALS rename (2.0.1.0 → 2.0.2.0) plus a
large doc-only touch-up pass (no source changes outside `docs/`). No review
needed; tracking ref updated to 177fa6c.

### sunoc/ninja11/v2.0.2.0 (2524ed6) — unchanged
No new commits since last seen.

### rares/meta5/v2.0.2.0 (e5db135 → 90f9ec1) — 4 commits
Reviewed. Summary below; see chat delivery for the full structured review.

1. `cc75da5` item-15: hermes_kawarimi Kawarimi adapter (Rarity, autonomous)
2. `64a5c8a` Merge PR #75 (item-15) into rares/meta5/v2.0.2.0
3. `1e6d9ed` Add docs/proposals/MT-ADMIN-DECOMPOSITION.md (sun)
4. `90f9ec1` Update docs/ops/AGENT_GOALS_2.0.2.0.md (mark items 14+15 done)

#### hermes_kawarimi — review notes

**Architecture (clean):** provision-then-populate. extract.py (read-only) →
HermesAgentSnapshot IR → mapper.py (pure, deterministic uuid5) → MappedAgent →
loader.py (--apply only) provisions a fresh tenant via mt-admin, boots it once
to let refinery create the schema, stops the daemon (PG stays up), populates
rows + encrypts secrets, then stages or starts. Reuses
`lunarwing_mt_onboard.secrets_ops` and `provisioner` — no reinvented crypto.

**Owner-scope safety:** every row written under the tenant's
`LUNARWING_OWNER_ID` read from its env after provisioning. No pg_restore, no
rekey → the KAWARIMI-OWNER-SCOPE-CONTINUITY silent-empty hazard structurally
cannot occur. Good design call.

**Determinism:** uuid5 with a fixed namespace means re-imports update rather
than duplicate. Verified in mapper_tests.py (test_uuid_determinism).

**Secret handling:** env keys classified via `_PROVIDER_MAP` + `_SECRET_HINTS`
(API_KEY/TOKEN/SECRET/PASSWORD/etc.). Names lowercased, validated against
`_SECRET_NAME_RE = ^[a-zA-Z0-9_/-]+$`. auth.json carried whole as
`hermes_auth_json` (encrypted) — avoids fragile per-field parsing of a
schema-variable file. Values never printed; dry-run report shows names only.

**Verified raw bytes** for the bits that looked redacted in tool output:
- `_SECRET_NAME_RE = re.compile(r"^[a-zA-Z0-9_/-]+$")` — real code, not a
  redaction artifact.
- `_SECRET_HINTS = (...)` tuple — real code.
- extract_tests.py writes `'ANTHROPIC_API_KEY="sk-abc123"\n...'` and
  auth.json `{"anthropic": {"token": "t"}}` — real fixture strings, not
  redacted variables. The earlier `***` in tool output was the Hermes
  secret-redaction artifact, NOT a bug.

**Concerns / observations:**

1. **`_populate` memory_documents upsert assumes `agent_id IS NULL`.** The
   UPDATE path hardcodes `agent_id IS NULL`; if LunarWing ever imports docs
   under a non-null agent_id, the UPDATE misses and a duplicate INSERT fires
   (the UNIQUE constraint treats NULL as distinct, so this won't error, but
   it diverges from the "re-import updates" contract for agent-scoped docs).
   Low risk today because the adapter always writes agent_id=NULL, but worth
   a comment linking the assumption to the UNIQUE semantics. (loader.py
   `_populate`, memory_documents block.)

2. **`_insert_secrets` has no ON CONFLICT handling.** Unlike
   memory_documents/conversations/settings (which all upsert),
   `secrets_ops.insert_secret` is called per-secret with no dedupe. A
   re-import will attempt to re-insert the same secret name. Behavior
   depends on `secrets_ops.insert_secret`'s own conflict handling (not in
   this diff) — if it doesn't upsert, re-import either errors per-secret
   (caught, warned) or creates duplicates. Should be verified against the
   sibling package before relying on re-import idempotency for secrets.

3. **`_wait_for_schema` 90s deadline.** Polls every 2s for the
   `memory_documents` table via `to_regclass`. Reasonable, but on a slow
   host (cold first-boot migrations + container pull) 90s could be tight.
   Not a bug; flagging as a tunable. The `_SCHEMA_WAIT_SECONDS`/`_SCHEMA_POLL_INTERVAL`
   constants are module-level, so easy to override by fork if needed.

4. **`ImportPlan.auto_yes` defaults to `True`** but is never read by
   `run_import` or `preflight` in this diff. Presumably consumed by the
   mt-admin calls downstream; harmless but the field is currently dead
   within this package.

5. **`with_toolchains` / `with_vision` on ImportPlan** are serialized and
   parsed but never wired into `_worker_flags()` / `_add_tenant_args()`.
   If the intent is to plumb them to add-tenant later, they should be; if
   not, they're dead fields. Minor.

6. **`_message_content` coalesces None content → tool_calls → "".** Correct
   for the NOT NULL constraint, but assistant messages with only a
   tool_name and no content/tool_calls become empty strings. Acceptable for
   v1; note for future enrichment.

**Test coverage:** 12 unit tests across model/extract/mapper. Extract test
builds a synthetic Hermes home with state.db, markdown, .env, auth.json,
config.yaml. Mapper tests cover content coalescing, uuid determinism, secret
classification/providers, auth.json roundtrip, settings namespacing. No tests
for loader.py (live path, needs root+DB — reasonable to defer). Mapper is
deterministic so the contract is well-covered.

**Cross-branch consistency:** hermes_kawarimi lives only on
rares/meta5/v2.0.2.0 (via merged PR #75 from rarity/item-15). Not on master
or sunoc. No conflict surface — it's an additive Python package under a new
top-level `hermes_kawarimi/` directory; doesn't touch Rust source, mt-admin,
or shared scripts. Clean merge candidate when rares lands.

#### docs/proposals/MT-ADMIN-DECOMPOSITION.md — review notes

Proposal-only (no code changes). Plans to split `lunarwing-mt-admin.sh`
(~7500 lines, 41 subcommands) into 4 required + 2 optional sourced bash
libraries under `ic/scripts/mt-admin-lib/`:

- ports-registry.sh (L884–1482, ~599 lines)
- env-generation.sh (L2168–2822, ~655 lines)
- units-systemd.sh (L4817–5294, ~478 lines)
- units-openrc.sh (L5297–5976 + L656–883, ~680+323 lines)
- health-pipeline.sh (optional, L6002–6200)
- tenant-lifecycle.sh (optional, add/remove/start/stop/upgrade/status/doctor)

**Strengths:**
- Grounded in actual line ranges from `rg -n '^# ── '` against the current
  file. The structural map table is the most useful artifact in the doc.
- Explicitly preserves the AGENTS.md HARD RULE: tenant lifecycle stays
  inside the mt-admin family (no bare `systemctl is-active` leaks).
- Calls out shared-state contracts per library (which symbols must be
  sourced first). Good discipline.
- Notes that systemd and OpenRC unit renderers are "near-mirror images" —
  keeping them as separate libraries (3a/3b) is the right call for
  reviewability.
- "Not rewriting in Python/Rust" is correct — keeps the onboard wrappers as
  thin drivers.

**Observations:**
- Counts units-systemd + units-openrc as two parts to hit "AT LEAST FOUR";
  this is reasonable but worth confirming sun agrees that 3a/3b count
  separately (the doc argues it well).
- The "~4700 lines after required splits" math checks out against the
  line ranges given.
- No mention of how `shellcheck` scoping or a test harness would attach to
  the new libraries — that's the stated motivation (§2.1) but the proposal
  doesn't sketch the test surface. Worth a follow-up section before
  implementation begins.
- Phase-2 `tenant-lifecycle.sh` extraction is flagged optional; given it's
  the 1301-line dispatcher + verbs, it's where the real reviewability win
  is. Recommend not deferring it indefinitely.

**Verdict:** solid proposal, ready for sun to greenlight implementation.
The line-range map alone is valuable institutional memory even if the split
isn't done immediately.

### Verdict
Only rares/meta5/v2.0.2.0 moved (4 commits, all additive: one new Python
package + one proposal doc + a goals-checkoff). No regressions, no security
issues, no Rust/mt-admin changes. hermes_kawarimi is well-structured with
good test coverage on the pure layers; the live loader path is
untested-in-tree (deferred to host rehearsal, which is appropriate). Two
minor idempotency concerns (memory_docs agent_id assumption, secrets upsert
behavior) worth verifying against secrets_ops before relying on re-import.
MT-ADMIN-DECOMPOSITION is a grounded, low-risk proposal ready for sign-off.
