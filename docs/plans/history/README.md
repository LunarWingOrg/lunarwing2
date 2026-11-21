# Historical implementation plans

These files are retained for provenance, not as an execution backlog. Their
original design assumptions, branch states and verification snippets may be
outdated. Read each status banner before using any part of a record.

| Record | Disposition as of 2026-11-20 | Current reference |
|---|---|---|
| [Engine V2 local compatibility status, 2026-07-15](ENGINE_V2_LOCAL_COMPATIBILITY_STATUS_2026-07-15.md) | Snapshot; subsequent compatibility work shipped | [v2.0.1.0 release notes](../../releases/RELEASE-v2.0.1.0.md) and [Engine V2 architecture](../../architecture/ENGINE-V2.md) |
| [Engine V2 skill-feedback live E2E plan, 2026-07-15](ENGINE_V2_SKILL_FEEDBACK_E2E_2026-07-15.md) | Completed on a disposable OpenRC tenant; tenant purged | [Same-day status record](SELF_IMPROVING_SKILLS_STATUS_2026-07-15.md) |
| [Near/IronClaw naming removal](Near-Removal.md) | Original plan marked done; pre-existing history entry. Its later audit found live verification and final stale-name evidence missing | [Archived specification](../../specs/history/Near-Removal.md) and [completion audit](../../ops/history/NEAR_REMOVAL_COMPLETION_AUDIT.md) |
| [OpenCode 2 external-worker exploration](OPENCODE2_EXTERNAL_WORKER_PLAN.md) | Implemented as a separate opt-in lane | [`opencode2.4lunarwing/README.md`](../../../opencode2.4lunarwing/README.md) and [worker operations](../../ops/WORKER-CONTAINERS.md) |
| [Self-improving skills B-2/B-3](SELF_IMPROVING_SKILLS_B2_B3.md) | Delivered; later registry-update fixes also shipped | [v2.0.1.0 release notes](../../releases/RELEASE-v2.0.1.0.md) and `ic/FEATURE_PARITY.md` |
| [Self-improving skills status, 2026-07-15](SELF_IMPROVING_SKILLS_STATUS_2026-07-15.md) | Point-in-time implementation and audit summary; not current findings | [v2.0.1.0 release notes](../../releases/RELEASE-v2.0.1.0.md) |
| [WeeChat relay bootstrap](WEECHAT-BOOTSTRAP.md) | Shipped in v2.0.1.0 | [WeeChat relay specification](../../specs/WeeChat-Relay.md) |

The active index is [`docs/plans/README.md`](../README.md). When archiving a plan,
add its completed or superseded state here and provide a link to current behavior
or a successor work item; do not delete the original evidence.
