# Critical Review: PR #23 and PR #24

> **Current status (2026-07-20, rev `50c8f99`): HISTORICAL RECORD.** This is a
> point-in-time review of two unmerged PRs. The compatibility symlinks and
> `ironclaw-agent-v1` legacy subprotocol remain in this checkout, so neither PR's
> proposed removal should be treated as shipped.

## Scope and method

This review compares each PR with the merge-base of the requested base branch,
`origin/rares/meta1/v2.0.0.0`:

- Merge-base: `c8352943b419330e0b49685a181c34474327956a`
- PR #23 head: `e4a9522b98703b308f35786149a3d93be9927c6e`
- PR #24 head: `ceb24ab14353a1b2b67769fba187aea362ddb1d3`

The origin base ref was present, so no fallback to the working-tree `HEAD` was
needed. The review is static: no Cargo build, test, lint, or runtime exercise
was performed. Line citations below use the PR head unless a base commit is
shown explicitly.

I use these status terms deliberately: **tracked** means a document mentions a
feature; **implemented** means code in this checkout supports it; **release
ready** means the upgrade and verification path accounts for it. Those states
are not interchangeable.

## Executive verdict

| PR | Verdict | Primary reason |
|---|---|---|
| #23 | **Needs rework** | The analysis uses nonexistent protocol names, misstates secret migration evidence, and calls several idea-level or unchecked items covered/shipped. It also omits the implemented Engine V2 learning-mission system. |
| #24 | **Needs rework** | The daemon change works for fresh current worker images, but the normal tenant upgrade path can leave a legacy-only worker container behind. The only legacy negotiation test was deleted and the replacement does not prove the exact offer. |

## PR #23 - Blog feature gap analysis

### What the PR claims and actually changes

The PR is documentation-only:

| File | Actual change |
|---|---|
| `docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md` | Adds a 236-line table and narrative claiming to cross-reference every blog feature against the goals and roadmap. It reports five gaps, a list of covered items, a non-blog cross-reference, and five recommendations. |
| `docs/ops/AGENT_GOALS_2.0.0.0.md` | Changes item 5 from unchecked to checked (`e4a9522:docs/ops/AGENT_GOALS_2.0.0.0.md:14`). |

There is no source or test change. The document is therefore not a runtime
regression, but it is intended to guide release scope and can create operational
or implementation regressions if its claims are copied into follow-up work.

### What is correct or useful

1. The roadmap does contain future entries for Lorebooks, profile switching,
   workspace seeding, self-healing, MCP, speech-to-text, and the Hermes-derived
   feature set (`e4a9522:docs/ops/ROADMAP_2026.md:14-30`). Calling those items
   future tracking rather than claiming that this PR implements them is the
   right general direction.
2. The PR correctly notices that item 6 concerns the legacy external-worker
   alias, and that the goals document does not itself spell out every future
   feature. The problem is the protocol name and the resulting conclusions,
   discussed below.
3. The recommendation to define user control, registration, and migration
   behavior for any genuinely new preconfigured behavior is sound once the
   existing Engine V2 missions and heartbeat behavior have been accounted for.

### Accuracy and correctness findings

#### P1 - The protocol names in the analysis are not the repository protocol

The summary and recommendation repeatedly use `lunarwing-websocket-1` and
`ironclaw-websocket-1` (`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:18,42-62`).
Neither string appears in the active code. The daemon calls the protocol
`lunarwing-agent-v1` and the legacy alias `ironclaw-agent-v1`
(`e4a9522:ic/src/orchestrator/external_worker.rs:28-33,748-752`). The worker
implementations use the same names, for example Pebble
(`e4a9522:pebble4lunarwing/src/protocol.rs:4-7`) and the TypeScript runtime
(`e4a9522:lunarcode4lunarwing/scripts/lunarwing_runtime.ts:112-116`). The v1.1.9
release notes also document that exact `agent-v1` rename
(`e4a9522:docs/releases/RELEASE-v1.1.9.0.md:13-16,35-41`).

This is more than terminology. The proposed action says that the workers and
their documentation need to adopt the nonexistent `lunarwing-websocket-1`
name. Following that recommendation would make the daemon and all current
workers fail negotiation. The remaining real work is the planned removal of the
`ironclaw-agent-v1` compatibility path, not adoption of a new protocol name.

#### P1 - The claimed database-secret migration is unsupported and risks operator confusion

The blog section is presented as fact: "The database secret has been renamed
with migrations seeded in 1.1.9" (`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:42-52`).
The checked-in migration with a rename is `V22__rename_leak_pattern.sql`, and it
renames only the leak-detection pattern `nearai_session` to
`lunarwing_cloud_session` (`e4a9522:ic/migrations/V22__rename_leak_pattern.sql:1-6`).
It does not rename a database secret or the encrypted-vault key. The tenant
admin code explicitly preserves `SECRETS_MASTER_KEY` because changing it would
orphan encrypted data (`e4a9522:ic/scripts/lunarwing-mt-admin.sh:2243-2253`),
and the keychain service is still named `ironclaw`
(`e4a9522:ic/src/secrets/keychain.rs:20-28`).

If the blog refers to an external migration not represented in this checkout,
the review document must label that claim unverifiable and link the authoritative
migration. As written, an operator could rotate or rename the wrong credential.

#### P1 - "Preseeded memory routines" ignores existing preseeded Engine V2 behavior and uses a nonexistent path

The PR narrows the blog statement to "Preseeded Memory Routines," says the
feature is absent from both tracking documents, and says the existing routine
system is `src/agent/routines/` (`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:25,88-107`).
The active v1 routine files are `ic/src/agent/routine.rs` and
`ic/src/agent/routine_engine.rs`; there is no `src/agent/routines/` directory.
More importantly, Engine V2 already creates preconfigured behavior: its
architecture document specifies three event-driven learning missions created at
project bootstrap (`e4a9522:docs/architecture/ENGINE-V2.md:194-212`), and the
runtime creates self-improvement, skill-extraction, conversation-insights, and
expected-behavior missions (`e4a9522:ic/crates/lunarwing_engine/src/runtime/mission.rs:650-783`).
The bridge invokes that bootstrap (`e4a9522:ic/src/bridge/router.rs:960-966`).

There is also existing heartbeat/workspace seeding: core files including
`HEARTBEAT.md` are seeded (`e4a9522:ic/src/workspace/mod.rs:1591-1608`), profile
data can generate a personalized checklist (`e4a9522:ic/src/profile.rs:837-861`),
and the heartbeat runner executes a checklist periodically
(`e4a9522:ic/src/agent/heartbeat.rs:1-24,311-333`). This does not prove that the
blog's exact desired defaults are complete, but it makes a "No/No, only
user-created routines" classification false and incomplete. The review should
separate v1 database `Routine`, v2 `Mission`, and heartbeat seeds before
proposing a duplicate design.

#### P1 - The "Instinct Over Turns" gap claims that existing self-improvement design does not exist

The proposal says there is no architecture proposal or design for
self-improvement checks integrated into the agent loop
(`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:135-174`). Engine V2
already documents the learning-mission architecture and a self-modifiable
orchestrator (`e4a9522:docs/architecture/ENGINE-V2.md:1-5,120-129,194-212`).
The self-improvement mission has an explicit cadence, success criteria, seeded
fix-pattern database, and prompt/orchestrator patching behavior
(`e4a9522:ic/crates/lunarwing_engine/src/runtime/mission.rs:650-714`;
`e4a9522:ic/crates/lunarwing_engine/prompts/mission_self_improvement.md:1-39`).

There may still be a real delta between those event-driven missions and the
blog's broader notion of persistent internal drives or checks on every turn.
The PR should define that delta and note that Engine V2 is opt-in via
`ENGINE_V2=true` (`e4a9522:docs/architecture/ENGINE-V2.md:1-5`), rather than
claiming that all self-improvement integration is absent.

#### P1 - "Covered" and "Shipped" conflate roadmap mentions, ideas, and released code

Several rows in the summary and "Items Verified as Covered" table are too strong
(`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:18-36,178-194`):

- Lorebooks and Profiles are one- and two-line idea stubs
  (`e4a9522:docs/proposals/LOREBOOKS.md:1-2`; `e4a9522:docs/proposals/Profiles.md:1-2`),
  and the roadmap explicitly says the grouped features still require further
  planning (`e4a9522:docs/ops/ROADMAP_2026.md:14`). `docs/README.md` labels both
  proposals "idea" (`e4a9522:docs/README.md:175-180`).
- DarkIRC key exchange is not implemented: the goal remains unchecked and the
  proposal says "Proposal only. None ... exist yet"
  (`e4a9522:docs/ops/AGENT_GOALS_2.0.0.0.md:78-79`; `e4a9522:docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md:1-4`).
- "ONNX Support" is broader than the roadmap entry, which is specifically future
  ONNX Runtime speech-to-text support (`e4a9522:docs/ops/ROADMAP_2026.md:24-27`).
  The active code search has no ONNX implementation; the transcription module
  exposes API-based providers (`e4a9522:ic/src/llm/transcription/mod.rs:1-11`),
  while the parity table still marks audio transcription, TTS, and incremental
  TTS playback absent (`e4a9522:ic/FEATURE_PARITY.md:245-261`).
- Interactive secret management and localhost web onboarding are marked
  "Shipped (1.1.9)" (`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:26,32,194`).
  The v1.1.9 release notes say the web version would be added in the future and
  only list the CLI (`e4a9522:docs/releases/RELEASE-v1.1.9.0.md:3-5,20-22`).
  The web package is present in this v2 checkout and has a secrets endpoint
  (`e4a9522:lunarwing_mt_onboard_web/README.md:1-12`;
  `e4a9522:lunarwing_mt_onboard_web/app.py:122-127`), so the defensible status is
  "implemented in this branch; v1.1.9 shipment not established," not "shipped."

The same ambiguity appears for MCP: item 18 is unchecked even though it contains
a "DONE" subsection (`e4a9522:docs/ops/AGENT_GOALS_2.0.0.0.md:81-99`). A reader
cannot tell whether "Covered" means mentioned, partially implemented, or ready
for release.

#### P2 - The five-gap count contradicts the document's own selection rule

The stated goal is to find features "NOT yet tracked in either document"
(`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:5-10`), but only
Preseeded Memory Routines is `No/No` in the summary. LunarVoice is tracked in
the roadmap, Reflex is partially tracked, the protocol is partially represented
by item 6, and Instinct is represented by the Hermes roadmap item
(`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:18-36`;
`e4a9522:docs/ops/ROADMAP_2026.md:8,24-30`). Reporting all five as equivalent
gaps makes prioritization misleading. The table needs separate categories such
as `untracked`, `tracked but under-specified`, `implemented but unreleased`,
and `implementation absent`.

#### P2 - Reflex and voice conclusions are assertions without implementation-status evidence

The Reflex section infers that "partial rewrites" must be a scope mismatch with
the roadmap's "polishing" wording (`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:111-131`),
but that is an interpretation of marketing language, not a verifiable code
requirement. The compiler already has an implementation, E2E coverage, and an
explicitly unfinished parity status (`e4a9522:ic/FEATURE_PARITY.md:194-197`;
`e4a9522:docs/ops/ISSUE_STATUS_REPORT.md:79-84`). The review should report those
facts and request measurable compiler requirements rather than declare the blog
and roadmap inconsistent.

Likewise, "Agents can speak" is not analyzed into speech-to-text, text-to-speech,
playback, and channel delivery. The roadmap only promises future speech-to-text
and later two-way communication (`e4a9522:docs/ops/ROADMAP_2026.md:26,31-33`),
while parity marks all relevant audio capabilities absent. The current section
should not call generic ONNX "covered."

#### P2 - Provenance and discoverability are insufficient for a completeness claim

The new file contains only a live URL and a compilation date
(`e4a9522:docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md:3-10`); the repository
does not contain a dated blog snapshot or a per-feature source citation. Thus
"cross-references every feature" cannot be independently reproduced from the
checkout. The PR also adds no index/backlink even though `docs/README.md` says to
update the index when adding files (`e4a9522:docs/README.md:318-332`).

### Risks and correctness impact

Because PR #23 changes only documentation, its risk is decision risk rather than
an immediate daemon regression:

1. An engineer following the wrong `*-websocket-1` recommendation could create a
   protocol incompatibility with every current worker.
2. An operator could treat the secret statement as a vault migration and lose
   access to encrypted tenant data.
3. Calling roadmap ideas and unchecked proposals "covered" can cause release
   gates to be closed without implementation or verification.
4. Omitting Engine V2 missions and heartbeat seeds can lead to duplicate,
   conflicting "preseeded routines" designs and an inaccurate estimate of the
   remaining Instinct work.

### Required improvements before merge

1. Replace every `lunarwing-websocket-1`/`ironclaw-websocket-1` reference with the
   actual `lunarwing-agent-v1`/`ironclaw-agent-v1` protocol, and scope the gap to
   legacy-offer removal and worker migration.
2. Remove or qualify the database-secret claim. Cite `V22` as the leak-pattern
   rename and explicitly preserve `SECRETS_MASTER_KEY`; link an external,
   authoritative migration only if one exists.
3. Re-audit preconfigured behavior across v1 `Routine`, v2 `Mission`, workspace
   seeds, profiles, and heartbeat. Describe the missing delta instead of using
   a nonexistent source path.
4. Reclassify every row with independent `tracking`, `implementation`,
   `release`, and `verification` columns. Correct the Lorebooks, Profiles,
   DarkIRC, ONNX, web-onboarding, and MCP statuses.
5. Add implementation and parity evidence for Reflex and LunarVoice, separating
   STT, TTS, playback, and channel delivery. Turn subjective "scope mismatch"
   language into measurable acceptance criteria.
6. Preserve a dated blog snapshot or feature-source appendix, add the proposal to
   the documentation index, and add backlinks from the goals/roadmap if this is
   the canonical analysis.

### Verdict for PR #23

**Needs rework.** The document is a useful starting inventory, but the protocol
and secret claims are materially unsafe, the status table is not evidence-based,
and its largest conceptual gap ignores substantial Engine V2 implementation.
Checking item 5 should wait until the corrected analysis is reproducible and
the remaining items are classified by implementation and release state.

## PR #24 - Legacy external-worker subprotocol removal

### What the PR claims and actually changes

| File | Actual change |
|---|---|
| `ic/src/orchestrator/external_worker.rs` | Removes `SUBPROTOCOL_LEGACY` and changes the request header from `lunarwing-agent-v1, ironclaw-agent-v1` to only `lunarwing-agent-v1` (`ceb24ab:ic/src/orchestrator/external_worker.rs:27-33,743-747`). |
| `ic/tests/external_worker_integration.rs` | Removes the legacy constant import, `LegacyOnly` policy, and the successful legacy-only test. `PreferNew` and `NewOnly` both now accept only the primary name. The file drops from 771 to 729 lines and from 12 to 11 Tokio tests; diff is 7 insertions/49 deletions. |
| `ic/scripts/lunarwing-mt-admin.sh` | Changes only the external-worker comment to stop advertising the alias (`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:2587-2594`). It does not add a worker rebuild or migration gate. |
| `docs/ops/AGENT_GOALS_2.0.0.0.md` | Checks item 6, combining protocol removal and compatibility-symlink removal (`ceb24ab:docs/ops/AGENT_GOALS_2.0.0.0.md:14-17`). |
| `darkirc_channel_for_ironclaw`, `ironclaw_weechat_wss` | Deletes two tracked mode-`120000` symbolic links whose targets are the LunarWing-named directories. They are not submodule pointers (submodules would be mode `160000`). `git diff --summary` reports `delete mode 120000` for both. |

No worker directory or gitlink changed, and `.gitmodules` is unchanged. The
description of these entries as "submodule pointers" is therefore inaccurate;
the compatibility-link deletions are the complete root-level cleanup in this
PR.

### Negotiation trace and what current workers do

The daemon is the WebSocket client in this path. Before PR #24 it offered two
comma-separated values, with the new value first
(`c8352943:ic/src/orchestrator/external_worker.rs:748-754`); after PR #24 it
offers exactly one (`ceb24ab:ic/src/orchestrator/external_worker.rs:743-747`).
`connect_async` then waits for the worker's `ready` message
(`ceb24ab:ic/src/orchestrator/external_worker.rs:773-826`).

The normal worker containers are WebSocket servers. Their upgrade handlers parse
the daemon's offer, prefer `lunarwing-agent-v1`, and echo the matched value:

- lunarcode: `ceb24ab:lunarcode4lunarwing/scripts/lunarwing_bridge.ts:163-181`
- opencode: `ceb24ab:opencode4lunarwing/scripts/lunarwing_bridge.ts:184-202`
- Pebble: `ceb24ab:pebble4lunarwing/src/bridge.rs:155-187`

Therefore a fresh image built from this checkout negotiates successfully. A
legacy-only server has no matching value when the daemon offers only the primary
name, returns an upgrade error, and never reaches `ready`. The current worker
sources also have an optional client/hub role that still offers both names
(`ceb24ab:lunarcode4lunarwing/scripts/lunarwing_bridge.ts:227-239` and
`ceb24ab:opencode4lunarwing/scripts/lunarwing_bridge.ts:248-260`); PR #24 does
not remove that role's alias. That is not the usual mt-admin daemon-to-worker
direction, but it is another reason to document the protocol boundary precisely.

### What is correct

1. Removing the constant and emitting a single primary offer is a coherent
   implementation of the stated daemon-side change.
2. All three current worker trees accept the primary name and echo it, so a
   clean deployment using current images should continue to work.
3. The two root entries really are compatibility symlinks, not submodules, and
   deleting them matches the v1.1.9 migration intent. The target directories
   remain present at the PR head.
4. The roadmap's sequencing says worker-side legacy acceptance is removed one
   release after the daemon stops offering it (`ceb24ab:docs/ops/ROADMAP_2026.md:7-9`).
   Keeping the worker aliases in this PR is therefore not itself a bug.
5. No authentication header, token value, envelope, or task state-machine logic
   changes. The risk is compatibility and upgrade orchestration, not an obvious
   auth bypass.

### Accuracy, risk, and lost-coverage findings

#### P0 - The standard tenant upgrade can leave a legacy-only worker running

The v1.1.8 release documentation describes the external worker as speaking only
`ironclaw-agent-v1` (`ceb24ab:docs/releases/RELEASE-v1.1.8.md:7-11,21-31`). The
repository's own mixed-version test plan calls out the "old worker + new daemon"
cell and says it requires rebuilding/recreating a v1.1.8 Pebble image
(`ceb24ab:docs/ops/TEST-PLAN-UPGRADED-TENANT-1.1.9.md:53-59,194-199`).

The normal `upgrade-tenant` path does not perform that migration:

1. `upgrade_tenant` checks out the target and calls `build_tenant "$name"
   "true"`, which enables WASM only (`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:6262-6322`).
   The common versioned upgrader has the same limitation: its apply path invokes
   only `build-tenant --with-wasm` (`ceb24ab:ic/scripts/upgrade-tenant-version.sh:343-356`).
2. Worker-image flags are separate optional arguments, defaulting to false; image
   builds occur only when `with_nanocode`, `with_pebble`, or `with_opencode` is
   explicitly true (`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:1734-1795`).
3. `start_tenant_*` reuses an existing container when its config hash matches
   (`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:3458-3479,3654-3672,3801-3819`).
4. The hash is computed from the env/quadlet config file, not the worker image
   ID or digest (`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:120-158`). Even a
   separately rebuilt `:latest` image can therefore leave an old container
   pinned to the legacy-only image.

After this PR, such a tenant's every external-worker task fails at the WebSocket
upgrade. With multiple endpoints, `wait=true` retries connection failures across
endpoints but `wait=false` launches one selected endpoint without retry
(`ceb24ab:ic/src/orchestrator/external_worker.rs:302-426`), so a mixed old/new
pool can appear intermittently healthy rather than fail clearly.

This is a release-blocking compatibility gap, not merely a theoretical old
client concern. The PR changes the daemon's accepted deployment contract without
proving that the supported upgrade tooling removes legacy-only containers.

#### P2 - Health and status checks cannot detect a protocol-incompatible image

The operational checks report a worker as running based on container state and,
for OpenRC, an internal `/health` response
(`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:674-678`). `status` likewise prints
running/stopped/not-created and `doctor` checks only that a `:latest` image tag
exists (`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:6373-6397,6603-6613`). None
performs a daemon-to-worker WebSocket handshake or compares the running
container's image ID with the current image. A legacy-only container can
therefore look healthy until the first job.

#### P1 - Operator documentation still says the protocol transition needs no action

`TENANT-RENAME-MIGRATION-1.1.9.md` says the subprotocol transition needs "no
operator action" because workers and daemon negotiate automatically
(`ceb24ab:docs/ops/TENANT-RENAME-MIGRATION-1.1.9.md:24-31`). That statement is
no longer true once the daemon stops offering the legacy value. The migration
guide also says the old protocol was intentionally not renamed
(`ceb24ab:docs/guides/MIGRATE_IRONCLAW_TO_LUNARWING.md:139-150`). Neither is
updated by this PR. Item 6's parenthetical mentions re-rendering units for the
deleted symlinks, but says nothing about rebuilding and recreating worker images
(`ceb24ab:docs/ops/AGENT_GOALS_2.0.0.0.md:15`).

An operator following the existing docs can perform a nominally successful
upgrade and discover only when a job is submitted that the worker cannot
negotiate. The migration gate and the docs must change together.

#### P1 - The test change deletes the only daemon integration regression test for the compatibility decision

The base test explicitly exercised a worker that accepted only
`ironclaw-agent-v1` and required the handshake and task to succeed
(`c8352943:ic/tests/external_worker_integration.rs:629-663`). PR #24 deletes that
test and the `LegacyOnly` policy. The surviving `PreferNew` and `NewOnly`
policies both return the identical one-element list
(`ceb24ab:ic/tests/external_worker_integration.rs:66-81`), so the former no
longer represents a distinct worker generation. The test suite loses 42 net
lines and one of 12 protocol tests without adding a replacement for the
failure mode introduced by the PR.

Deleting a test can be correct when behavior is intentionally removed, but then
the replacement should assert the new contract: a legacy-only worker must fail
cleanly, and a migration test must prove that supported upgrades never leave one
behind. Neither exists here.

#### P1 - The replacement test does not prove the exact request header

The mock imports the production `SUBPROTOCOL` constant and builds both policies
from it (`ceb24ab:ic/tests/external_worker_integration.rs:35-39,75-81`). A change
to the production constant and the mock can therefore agree on the wrong value.
The handshake callback also validates only inside `if let Some(offer)` and would
accept a request with no protocol header at all
(`ceb24ab:ic/tests/external_worker_integration.rs:206-232`). It does not assert
that the raw header is exactly `lunarwing-agent-v1`, nor that the legacy token is
absent. The remaining "new-only" test would pass against the base daemon's
two-name offer as well, so it cannot distinguish PR #24 from its parent.

#### P2 - Symlink removal remains warning-only for `--skip-render` upgrades

The upgrade command intentionally permits `--skip-render`, then starts the
tenant and only warns if old unit paths remain
(`ceb24ab:ic/scripts/lunarwing-mt-admin.sh:6312-6333`). Once the two symlinks are
deleted, an old unit can point at a path that no longer exists. The operator
documentation states this deadline, but the command does not fail closed. This
is a separate adapter outage risk from the WebSocket protocol risk and should be
covered by a preflight or an explicit v2 refusal.

### Required improvements before merge

1. Add a migration preflight that enumerates each selected tenant worker,
   identifies its image/version or performs a protocol handshake, and refuses
   the daemon upgrade unless every worker accepts `lunarwing-agent-v1`. The
   preflight must cover nanocode/lunarcode, Pebble, and opencode.
2. Make `upgrade-tenant` rebuild and recreate selected worker containers as part
   of the protocol transition, or require explicit, documented worker flags and
   fail if they are omitted. Include opencode in the older upgrade wrappers.
3. Include the image ID/digest in container freshness tracking, or force-remove
   and recreate worker containers after a worker image build. Updating a tag in
   the image store is not sufficient while an old container exists.
4. Extend `status`/`doctor` or the health pipeline with a protocol handshake and
   image identity/capability report, so a stale legacy worker is observable
   before traffic is sent.
5. Update `TENANT-RENAME-MIGRATION-1.1.9.md`,
   `MIGRATE_IRONCLAW_TO_LUNARWING.md`, worker guides, and the goals item with a
   concrete v2 migration sequence and a verification command. Remove the "no
   operator action" statement.
6. Replace the deleted success test with two independent tests: a mock that
   accepts only the literal `ironclaw-agent-v1` and must fail with a connection
   error, and a mock that captures the request and asserts the exact literal
   `lunarwing-agent-v1` with no legacy token. Require a protocol header rather
   than accepting `None`.
7. Add an upgrade-level test for a stale legacy image/container, including the
   mixed-endpoint `wait=true` and `wait=false` behavior. A pure loopback mock is
   not enough to validate the mt-admin lifecycle regression.
8. Make `--skip-render` incompatible with a v2 target, or run a fail-closed
   preflight before deleting compatibility links. Keep the warning only for
   upgrades that still have the links available.
9. Document the server/client role distinction and decide when the worker-side
   client-role alias is removed, so the roadmap and all protocol implementations
   describe one migration contract.

### Verdict for PR #24

**Needs rework.** The daemon edit is technically correct for fresh current
workers, and deleting the mode-`120000` compatibility symlinks matches the
intended v2 cleanup. However, the supported upgrade path does not ensure that
legacy-only worker containers are replaced, existing migration docs are now
misleading, and the test reduction removes the only direct coverage of the
compatibility boundary while leaving a redundant mock. Merge should wait for a
fail-closed worker migration/preflight and independent negotiation tests.

## Cross-PR interaction

Both PRs edit adjacent lines in `docs/ops/AGENT_GOALS_2.0.0.0.md`, so they should
be rebased before integration. More importantly, PR #23's invented
`lunarwing-websocket-1` naming conflicts with PR #24's actual
`lunarwing-agent-v1` contract. Merging both as written would simultaneously
check off the work and publish contradictory protocol guidance. Correct PR #23's
analysis and complete PR #24's migration/test work before treating either
checkbox as release evidence.
