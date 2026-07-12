# Open PRs and Issues — LunarWing v2.0.0.0

**Last updated:** 2026-07-12  
**Repository:** [LunarWing/LunarWing_v2](https://codeberg.org/LunarWing/LunarWing_v2) (Codeberg, primary)  
**GitHub mirror:** [LunarWingOrg/lunarwing2](https://github.com/LunarWingOrg/lunarwing2) — syncs every 12 hours, no independent PRs/issues

---

## Open Pull Requests (2)

### PR #24 — Item #6: Drop legacy `ironclaw-agent-v1` subprotocol offer
- **Author:** rarity6969
- **Branch:** `rarity/item-6-20260712-1200` → `rares/meta1/v2.0.0.0`
- **Created:** 2026-07-12
- **Status:** Open, mergeable
- **Files changed (6):** `ic/src/orchestrator/external_worker.rs`, `ic/scripts/lunarwing-mt-admin.sh`, `ic/tests/external_worker_integration.rs`, `docs/ops/AGENT_GOALS_2.0.0.0.md`, plus removal of compat symlinks (`darkirc_channel_for_ironclaw`, `ironclaw_weechat_wss`)
- **Summary:** Removes the legacy `SUBPROTOCOL_LEGACY` offer from the external worker daemon and deletes the 1.1.9-era compat symlinks. Deployed tenants must re-run mt-admin unit regen after merge.

### PR #23 — Item #5: V2 blog feature gap analysis
- **Author:** rarity6969
- **Branch:** `rarity/item-5-20260712-0901` → `rares/meta1/v2.0.0.0`
- **Created:** 2026-07-12
- **Status:** Open, mergeable
- **Files changed (2):** `docs/proposals/V2_BLOG_FEATURE_GAP_ANALYSIS.md` (+236 lines), `docs/ops/AGENT_GOALS_2.0.0.0.md`
- **Summary:** Analyzes features from the v2 launch blog post not yet tracked in `AGENT_GOALS_2.0.0.0.md` or `ROADMAP_2026.md` and documents them as proposals.

---

## Open Issues (0)

No open issues on the Codeberg tracker. The GitHub mirror also reports zero open issues.

---

## Recently Merged/Closed PRs (Context)

| PR | State | Title | Author |
|----|-------|-------|--------|
| #22 | Merged | sync rares/meta1/v2.0.0.0 with master | cmcsun |
| #21 | Merged | Add sprites/old.md | cmcsun |
| #20 | Closed | sync master with rares/meta1/v2.0.0.0 | cmcsun |
| #19 | Merged | sync master with rares/meta1/v2.0.0.0 | cmcsun |
| #18 | Merged | mv sloptegration6/sun/upgrade/v2.0.0.0 to rares | cmcsun |
| #17 | Merged | sync dev auto worker with master | cmcsun |
| #16 | Merged | Delete 26.1.2 | cmcsun |
| #15 | Merged | sync master with rares/meta1/v2.0.0.0 | cmcsun |
| #14 | Merged | sync with master | cmcsun |
| #13 | Merged | Delete docs/ops/TEST_FILE.md | cmcsun |
| #12 | Merged | added test file | cmcsun |
| #11 | Merged | sun/meta5/v2.0.0.0 | cmcsun |
| #10 | Merged | proposalsadded | cmcsun |
| #9 | Closed | Item #5: make sure you DIDNT MISS ANY CRATES | rarity6969 |
| #8 | Merged | Item #4: bump crates version from 1.1.9 to 2.0.0 | rarity6969 |
| #7 | Closed | Item #3: PARTIAL — agent stuck | rarity6969 |
| #6 | Merged | sun/meta2/v2.0.0.0 | cmcsun |
| #5 | Merged | sloptegration2/upgrade/v2.0.0.0 | cmcsun |
| #4 | Merged | Item #2: create ov dir | rarity6969 |
| #3 | Merged | goals1/v2.0.0.0 | cmcsun |

---

## Active Development Branches (Not Yet PR'd)

These local/remote branches represent in-progress work that has not yet been opened as a PR:

| Branch | Notes |
|--------|-------|
| `rarity/item-9-20260712-1500` | This item (open PRs/issues doc) |
| `feat/lunarwing_mt_onboard_web` | MT admin web onboarding UI (item #8, noted as worked on by another agent/human) |
| `feat/lunarwing_mt_onboard_web_c` | Variant of MT onboard web |
| `feat/lunarwing_mt_onboard_web_g` | Variant of MT onboard web |
| `feat/mt-onboard-web-import` | MT onboard web import feature |
| `integration/features/v2.0.0.0` | Feature integration branch |
| `integration/upgrade/v2.0.0.0` | Upgrade integration branch |
| `goals1/v2.0.0.0` | Earlier goals branch (merged via PR #3) |
| `goalsint2` | Goals integration branch |
| `postv1.1.9.0clean` | Post-1.1.9 cleanup branch |
| `slopmcp1/codex/upgrade/v2.0.0.0` | MCP additions work (item #18 reference) |
| `faility/failed-partial-old-item-3-20260711-0601` | Failed partial work (item #18 reference) |

---

## Notes

- The base branch for all v2.0.0.0 agent-goal PRs is `rares/meta1/v2.0.0.0`, not `master`. The `master` branch receives periodic syncs from this integration branch.
- Two earlier PRs (#7, #9) were closed without merging due to agent stuck states; their work was superseded by later successful attempts.
- The GitHub mirror at `LunarWingOrg/lunarwing2` does not accept PRs or issues independently — all development happens on Codeberg.
