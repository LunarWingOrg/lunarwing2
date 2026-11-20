---
plan name: SELF_IMPROVING_SKILLS_B2_B3
plan description: Skill confidence-based demotion/pruning (B-2) and cross-agent skill sharing (B-3)
plan status: delivered
---

# B-2 + B-3 — Skills ecosystem: demotion/pruning and cross-agent sharing

Follow-on to **B-1** (self-improving skills, delivered — see
`docs/proposals/SELF_IMPROVING_SKILLS_B1.md`). B-1 added the propose→approve
patch loop and the `SkillMetrics`/`patch_history`/`pending_patch` substrate.
This plan covers the two deferred items from the Hermes "Skills Ecosystem"
category:

- **B-2** — confidence-based **demotion** and **pruning** of rotten skills.
- **B-3** — **cross-agent skill sharing** via a registry (ClawHub).

Both reuse B-1 primitives. B-2 should land first (it is small and self-contained
and improves the signal B-3 relies on); B-3 is larger and has an external
dependency (a publish-capable registry).

---

## Current-state findings (verified in-tree)

- `SkillMetrics::confidence()` = success ratio, `is_patch_candidate()` added in
  B-1. (`ic/crates/lunarwing_skills/src/v2.rs`)
- **Confidence already de-weights ranking**:
  `selector::apply_confidence_factor(base, confidence, is_authored)` scales an
  *extracted* skill's score by `0.5 + 0.5*confidence` (authored skills exempt).
  So a low-confidence skill is already ranked lower — but it is **never fully
  excluded**, and nothing prunes dead skills. (`.../src/selector.rs`)
- **`SkillTrust` has exactly two variants** (`Installed=0 < Trusted=1`) with a
  SAFETY comment: ordering drives the security `min()` attenuation model. **Do
  NOT add a `Deprecated` trust variant** — demotion must be a separate flag, not
  a trust tier.
- **Cron missions exist**: `MissionCadence::Cron { expression, timezone }` plus a
  a 60s cron ticker (`start_cron_ticker`), and `ensure_learning_missions` /
  `ensure_self_improvement_mission` register per-user missions. A periodic
  pruning sweep fits this exactly.
- **ClawHub catalog already does PULL/SEARCH** (read-only):
  `catalog.rs` (`SkillCatalog::search`, detail, stars/downloads), behind the
  `catalog` feature (default-on). API handlers `skills_search_handler` /
  `skills_install_handler` exist. **There is no PUBLISH path and no
  trust-scored/versioned pull.**

---

# B-2 — Confidence-based demotion & pruning

## Goal
Stop low-confidence extracted skills from auto-activating (demotion), and remove
skills that are confidently dead (pruning) — both conservative and surfaced to
the user, never silently destructive.

## Design

### 1. Demotion = an activation floor + explicit `deprecated` flag (NOT a trust tier)
- Add to `V2SkillMetadata` (serde-default, back-compat):
  `deprecated_at: Option<DateTime<Utc>>` and `deprecation_reason: String`.
- Selector: a skill with `deprecated_at.is_some()` is **excluded from
  auto-activation** (still explicitly invocable by name / `/skill`), and
  `apply_confidence_factor` gains a hard floor — below
  `SKILL_DEMOTE_CONFIDENCE` (e.g. 0.3) with `usage_count >= min`, the extracted
  skill is not auto-selected. This is the demotion mechanism; trust is untouched.
- Demotion is **reversible**: a demoted skill that later earns confidence back
  (e.g. after a B-1 patch epoch-reset) clears `deprecated_at`.

### 2. Pruning = mark-for-deletion, user-confirmed (reuse B-1 propose→approve)
- After N consecutive/dominant failures with no successes and
  `usage_count >= min`, mark the skill with a pending **deletion proposal**
  rather than deleting. Reuse the exact B-1 pattern: a `pending_prune: bool` (or
  reuse `pending_patch` shape with a `kind`) surfaced through the same
  proposals API/GUI, approve = delete (or archive), reject = clear + reset.
- Never auto-delete. `Installed` skills are never pruned (read-only, external).
  `Trusted` user-authored skills are never auto-demoted/pruned (authored intent
  is respected — mirror `apply_confidence_factor`'s `is_authored` exemption).

### 3. Periodic sweep (cron mission)
- Add a per-user learning mission (via `ensure_learning_missions`) on a
  `MissionCadence::Cron` (e.g. daily) whose goal is: scan the user's extracted
  skills, demote those below the floor, and stage prune proposals for dead ones.
  Prompt: `prompts/mission_skill_maintenance.md` (new), conservative, one action
  per skill, propose-only for deletion.
- Alternative/complement: an on-skill-usage-record check that flags demotion
  inline (cheaper, no cron). Recommend BOTH: inline demotion (immediate),
  cron sweep (catches stale + stages prunes).

### 4. Notify + surface
- Extend the B-1 proposals GUI/API to show demotions and prune proposals
  alongside patch proposals (a `kind` field: patch | demote | prune).
- Optionally emit a Gotify/notification when a skill is demoted or a prune is
  staged (reuse existing notification path).

## Files (B-2)
- `ic/crates/lunarwing_skills/src/v2.rs` — `deprecated_at`, `deprecation_reason`,
  demote/prune consts, helpers (`is_demote_candidate`, `is_prune_candidate`).
- `ic/crates/lunarwing_skills/src/selector.rs` — exclude deprecated from
  auto-activation; hard floor in `apply_confidence_factor` path.
- `ic/crates/lunarwing_engine/src/memory/skill_tracker.rs` — `demote_skill`,
  `undeprecate_skill`, `propose_prune`, `apply_prune` (delete/archive),
  `discard_prune`.
- `ic/crates/lunarwing_engine/prompts/mission_skill_maintenance.md` — new sweep.
- `ic/crates/lunarwing_engine/src/runtime/mission.rs` — register the maintenance
  mission in `ensure_learning_missions`; sweep logic.
- `ic/src/bridge/router.rs` + web handlers — extend the proposals surface with
  `kind` (patch|demote|prune) and a prune approve/reject (reuse B-1 endpoints or
  add `/api/skills/proposals`).
- GUI: extend the proposals panel to render demote/prune cards.

## Tests (B-2)
- selector: deprecated skill excluded from auto-activation but still explicitly
  invocable; below-floor extracted skill not auto-selected; authored exempt.
- tracker: demote sets/clears `deprecated_at`; `propose_prune` stages without
  deleting; `apply_prune` deletes/archives; Installed/authored never pruned.
- sweep: dead extracted skill → prune proposal; healthy → untouched.

---

# B-3 — Cross-agent skill sharing via a registry

## Goal
Let one LunarWing instance publish trusted, high-confidence skills and let others
pull community-vetted skills with a trust score and versioned updates. Builds on
the existing (pull-only) ClawHub catalog.

## Design

### 1. Publish (the missing half)
- Add `SkillCatalog::publish(skill, opts)` (behind the `catalog` feature) that
  POSTs a skill's prompt + code_snippets + metadata (name, version,
  description, activation) to the registry publish endpoint with a
  `CLAWHUB_TOKEN`.
- Only **`Trusted`** skills with **confidence above a publish threshold** and a
  minimum `usage_count` are publishable (don't share unproven/rotten skills).
  Never publish secrets: run the existing `LeakDetector::scan` over the skill
  body + snippets before publish; refuse on hit.
- API: `POST /api/skills/{name}/publish`; GUI: a "Publish" action on eligible
  skill cards. Auth-gated, user-scoped, explicit (never automatic).

### 2. Trust-scored, versioned pull
- Extend the catalog entry with a registry trust/vet score + version, already
  partially present (`stars`, `downloads`, `version`, `updated_at`).
- Pulled skills install at `SkillTrust::Installed` (read-only tools only —
  matches the existing security model; external code is never `Trusted`).
- **Versioned updates**: track the installed registry version + `content_hash`;
  a periodic/`on-demand` check flags when a newer registry version exists and
  offers an update through the B-1/B-2 proposals surface (propose→approve,
  never silent). Reuse `content_hash` for change detection.

### 3. Provenance + safety
- Record on install: source registry URL, publisher, version, pulled-at,
  registry content hash (into `V2SkillMetadata`, new serde-default fields).
- Re-run `LeakDetector::scan` + the existing skill `validation.rs` on any pulled
  skill before it becomes active. Pulled skills can never auto-escalate trust.

## Files (B-3)
- `ic/crates/lunarwing_skills/src/catalog.rs` — `publish`, version-check,
  publish-eligibility helper.
- `ic/crates/lunarwing_skills/src/v2.rs` — provenance fields (registry_url,
  publisher, registry_version, pulled_at, registry_content_hash).
- `ic/src/channels/web/handlers/skills.rs` + `server.rs` — publish route,
  update-available surfacing.
- `ic/src/secrets/*` or env — `CLAWHUB_TOKEN` handling (never in argv/logs;
  follow existing secret conventions).
- GUI: Publish action + "update available" badge on installed skills.

## Tests (B-3)
- publish eligibility: only Trusted + above-threshold + min-usage; leak-scan
  refuses a skill containing a credential pattern.
- pull installs at `Installed` trust; provenance recorded; validation runs.
- version-check flags a newer registry version as an update proposal.

---

## Sequencing & constraints
- **Order:** B-2 first (small, improves signal, reuses B-1 surface), then B-3.
- Both extend B-1's propose→approve model and its proposals API/GUI rather than
  inventing new UX — add a `kind` discriminator (patch | demote | prune |
  update) to the pending-proposal shape.
- **SkillTrust is not modified** — demotion is a metadata flag, not a trust tier
  (safety-critical ordering).
- Build: `taskset -c 0-5 cargo check -j6` / `cargo test -p lunarwing_skills -p
  lunarwing_engine`. No full debug builds. Static assets are `include_bytes!`-
  compiled (GUI needs a release rebuild to take effect).
- Nothing is auto-destructive: demotion is reversible, pruning and updates are
  user-approved, `Installed`/authored skills are protected.

## Open questions (resolved)
- **Demotion floor / prune criteria** → `DEFAULT_DEMOTE_CONFIDENCE = 0.3` /
  `DEFAULT_DEMOTE_MIN_USAGE = 5`; `DEFAULT_PRUNE_CONFIDENCE = 0.0` /
  `DEFAULT_PRUNE_MIN_USAGE = 10`. Prune is strictly worse than demote (0.0 over
  more uses). Exposed as named consts in `v2.rs`; tune with real data.
- **Delete vs archive** → **archive** (soft delete via `archived_at`). Matches
  the never-delete retention ethos; the MemoryDoc is retained for
  audit/recovery.
- **ClawHub publish feasibility** → **confirmed feasible.** ClawHub
  (`openclaw/clawhub`, Convex backend at `wry-manatee-359.convex.site`, the same
  host LunarWing already pulls from) supports `POST /api/v1/skills`
  (`publishSkillV1Handler`) with `multipart/form-data` (`payload` JSON + `files`
  blobs), Bearer-token auth, and server-side `/api/v1/skills/-/scan` secret
  scanning. Versioned update detection via `GET /api/v1/resolve?slug=&hash=`.
  Self-hosting is **not** required for publish; a private fleet can point
  `CLAWHUB_REGISTRY` at a ClawHub-compatible endpoint (e.g. SkillHub) for free.
- **Trigger model** → `OnSystemEvent` on `thread_completed_with_issues` (NOT
  cron). Key finding: `MissionCadence::Cron` missions don't fire in production
  today (`next_fire_at` is never populated for missions — only the routine
  subsystem computes cron fire times). The maintenance sweep rides the same
  event as self-improvement; inline demotion in `record_usage` handles the
  immediate below-floor case.

---

## Implementation summary (2026-07-13) — DELIVERED

B-2 and B-3 are implemented end-to-end (engine + bridge + API + GUI), all layers
verified with scoped `cargo test`/`cargo check` and JS syntax checks. Branch
`feat/skills-b2-b3-01`.

### Shared surface (Part A — generalized the B-1 patch-only surface)
- **Data** (`v2.rs`): `ProposalKind` enum (patch | prune | update, default =
  patch for back-compat).
- **Bridge** (`router.rs`): `SkillProposal` DTO (kind-discriminated,
  kind-specific fields `Option`/`skip_serializing_if`),
  `list_pending_skill_proposals` / `approve_skill_proposal` /
  `reject_skill_proposal` (dispatch on kind). B-1 `SkillPatchProposal` /
  `list_pending_skill_patches` / `approve_skill_patch` / `reject_skill_patch`
  kept as thin back-compat wrappers.
- **API** (`handlers/skills.rs` + `server.rs`): `GET /api/skills/proposals`,
  `POST /api/skills/proposals/{doc_id}/{approve,reject}` (body `{kind}`).
  `/api/skills/patches*` kept as aliases.
- **GUI** (`app.js` + i18n en/zh-CN): panel generalized to render patch/prune/
  update cards via a `kind` badge + per-kind approve label.

### B-2 — demotion & pruning
- **Data** (`v2.rs`): `deprecated_at`/`deprecation_reason`,
  `archived_at`/`archived_reason`, `pending_prune: Option<PendingSkillPrune>`,
  `is_demote_candidate` / `is_prune_candidate` predicates (authored/Installed
  exempt; SkillTrust untouched), threshold consts.
- **Runtime** (`orchestrator.rs`): `handle_list_skills` excludes
  deprecated/archived skills — the single choke point (user decision). Python
  scorer's confidence math left untouched.
- **Storage** (`skill_tracker.rs`): `demote_skill` / `undeprecate_skill` /
  `propose_prune` / `apply_prune` / `discard_prune`. Inline demotion in
  `record_usage` (auto, reversible below the floor).
- **Mission** (`mission.rs` + `prompts/mission_skill_maintenance.md`): 5th
  learning mission (`skill_maintenance`, `OnSystemEvent`, max 1/day) +
  `collect_prune_candidate_skills` enrichment.
- **Host fn** (`orchestrator.rs`): `__propose_skill_prune__`.

### B-3 — cross-agent sharing
- **Dep** (`lunarwing_skills/Cargo.toml`): `lunarwing_safety` optional behind
  `catalog`; `reqwest` gains `multipart`.
- **Data** (`v2.rs`): provenance fields (`registry_url`/`registry_publisher`/
  `registry_version`/`pulled_at`/`registry_content_hash`),
  `pending_update: Option<PendingSkillUpdate>`, `is_publish_eligible`,
  publish consts.
- **Catalog** (`catalog.rs`): `publish()` (multipart `payload`+`files` →
  `POST /api/v1/skills`, Bearer), `check_for_update()` (`GET /api/v1/resolve`),
  `leak_scan()` (defense-in-depth pre-flight). Fixed `SkillDetail.version`
  (now populated from `latestVersion`, previously dropped → always `None`).
- **Provenance** (`skill_migration.rs`): `.registry.json` sidecar read at v1→v2
  migration stamps provenance into `V2SkillMetadata`.
- **Install** (`handlers/skills.rs`): leak-scan on pull; sidecar written for
  catalog pulls.
- **Storage** (`skill_tracker.rs`): `propose_update` / `apply_update` /
  `discard_update`.
- **Bridge** (`router.rs`): `publish_skill` (eligibility + leak-scan body &
  snippets + `CLAWHUB_TOKEN` from env, never logged) +
  `SkillPublishResult` DTO.
- **API** (`handlers/skills.rs` + `server.rs`): `POST /api/skills/publish/{doc_id}`.
- **GUI** (`app.js` + i18n): Publish button on trusted skill cards; update-kind
  proposal card already rendered via Part A.

### Verification
- `cargo test -p lunarwing_skills`: 161 passed (incl. 17 new B-2/B-3 tests).
- `cargo test -p lunarwing_engine`: 327 passed (incl. 13 new B-2 tests:
  demote/prune tracker + prune-candidate collectors).
- `cargo clippy --all --benches --tests --examples -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.
- Skills auth regression (`test_skills_handlers_require_auth`): covers the 4
  new routes (`/api/skills/proposals*` + `/api/skills/publish/{doc_id}`).
- JS syntax checks (`app.js`, `en.js`, `zh-CN.js`): clean.

### Decisions honored
- Demotion = metadata flag, NOT a trust tier (SAETY comment on `SkillTrust`
  ordering preserved).
- Demotion hook in Rust `handle_list_skills` (single choke point), Python
  scorer untouched.
- Sweep via `OnSystemEvent` (cron-mission gap avoided — not in scope to fix).
- Prune = archive (soft delete); `Installed`/authored never pruned/demoted.
- Publish explicit + eligibility-gated + leak-scanned; `CLAWHUB_TOKEN` secret.
- Pulled skills install at `SkillTrust::Installed` (existing security model).

### Note
Gateway static assets are `include_bytes!`-compiled — the GUI (Publish button,
unified proposals panel) requires a release rebuild / next tenant build to
appear.

### Deferred
- Live HTTP integration tests against ClawHub (publish/resolve) — `#[ignore]`-
  gated, need a real `CLAWHUB_TOKEN`.
- Update-application content pull (the bridge stamps provenance + bumps version
  on approval, but does not yet re-download the newer SKILL.md body — that
  needs the install path wired into the approve flow; the proposal surface is
  in place).
- Gotify notification on demotion/prune (the `MissionNotification` broadcast +
  `notify_channels` surface exists; wiring a channel name is a one-line
  follow-up).
