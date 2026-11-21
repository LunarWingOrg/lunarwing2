# Implementation plans

This directory separates **remaining work** from **historical design and status
records**. File age alone does not make a plan obsolete; its verified completion
state and tracked follow-ups determine where it belongs.

## Active plans

| Plan | Status as of 2026-11-20 | Purpose and current authority |
|---|---|---|
| [Engine V2 channel-parity work items](ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md) | Partial; revalidation required | Implementation, acceptance criteria and evidence for CHPAR-001–012. CHPAR-001–011 are recorded complete. **CHPAR-012 remains unfinished in this record**: its WeeChat slice passed on 2026-07-19; DarkIRC, XMPP and tenant teardown were not recorded. No later completion was found during this reorganization. See the [live-validation record](../reviews/ENGINE_V2_CHANNEL_PARITY_LIVE_VALIDATION_2026-07-19.md). |
| [Runtime EMA estimation](WI191_PERFORMANCE_EMA_INTEGRATION.md) | Initial slice shipped; follow-ups planned | Explains the per-user tool-runtime estimates and opt-in approval hints shipped in v2.1.4.0. [#191](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/191) is closed. [#318](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/318) tracks diagnostics and tenant evaluation, then LLM, gate-wait and bounded job/mission-run estimates, with a durable cache only if justified. Current behavior is specified by [`ic/src/estimation/README.md`](../../ic/src/estimation/README.md). |

## Historical records

[The history index](history/README.md) explains each archived plan. These records
preserve original proposals, dated implementation claims and test evidence; they
are not active task lists. Dated files have descriptive names rather than labels
such as `STATUSTODAYWED`.

- **Implemented:** OpenCode 2 worker exploration, WeeChat relay bootstrap, and
  self-improving skills B-2/B-3.
- **Completed validation and status snapshots:** Engine V2 local compatibility,
  skill-feedback live E2E, and the original B-1/B-2/B-3 status record.
- **Pre-existing archive:** Near/IronClaw naming-removal plan, whose original
  done marker was not accepted as completion evidence by its later audit.

## Maintenance rules

1. Keep a plan here while it has actionable, unshipped work. State its tracker,
   purpose, last verified status and remaining gates near the beginning.
2. Distinguish design intent, implementation and live validation. An old plan is
   not evidence that its checkboxes or claims still describe current source.
3. Move delivered plans and superseded status snapshots to `history/` with a
   banner linking to the current implementation, specification or successor work
   item. Preserve their historical text and evidence.
4. Name dated status records `SUBJECT_STATUS_YYYY-MM-DD.md`; give durable plans a
   subject-based name. Do not create undated `TODAY`, `WED`, `PHASEN` or opaque
   shorthand filenames.
5. Use GitLab work items for execution ownership and prioritization. Do not add
   placeholder plans; proposals for undecided work belong in `docs/proposals/`.
6. Update this index, [the documentation index](../README.md), inbound links and
   the archive index whenever a plan is added, moved or completed. Do not rewrite
   old release notes merely to relocate their historical references.

Superpowers retains its separately indexed plans in
[`docs/superpowers/plans/`](../superpowers/plans/). Subsystem contracts remain
beside their source; this directory does not replace those specifications.

Reorganized for [work item #794](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/794).
