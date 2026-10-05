# Archived operations records

This directory contains superseded checklists, release-specific evidence, review
snapshots, and scratch notes retained for provenance. These files are **not
maintained** and may be inaccurate against the current tree; do not use them as
operational procedures.

## Layout

- `GOALS_*.md` and `AGENT_GOALS_*.md` are completed or superseded release
  checklists. Checkbox state is historical.
- `v2.0.1.0/` through `v2.0.8.0/` contain release-bound audits, validation
  reports, test plans, and codename notes.
- Other files in this directory are point-in-time reviews or migration-era
  scratch records whose filenames still carry useful provenance.
- Flat `v2.1.4.0_*.md` records, matching the preceding v2.1.x layout, hold Kabuto security audits, work-item
  reconciliation, codename shortlist, and implementation verification records.
- The 2026-10-05 housekeeping pass also moved dated CI/SIS/REPL/DarkIRC
  records, the original Gentoo setup and 1.1.9 acceptance plan, upstream
  research, and resolved security findings here. Any remaining follow-ups
  in those snapshots describe their recorded baseline, not current status.
- Existing cited CI/export/repair excerpts are retained as historical evidence;
  the empty CI placeholder was deleted. New evidence should link to CI jobs.
- Older archived ops material also exists under
  [`docs/internal/history/archive/ops/`](../../internal/history/archive/ops/).

The maintained entry point is [`docs/ops/README.md`](../README.md). The active
release checklist is the `AGENT_GOALS_*.md` file kept in the parent directory;
current long-range planning lives in [`ROADMAP_2026.md`](../ROADMAP_2026.md).

When archiving another release, move its completed evidence into a versioned
record matching the adjacent release's layout and repair repository links. Do not add raw CI transcripts or
one-line placeholders as archival records; Git history already preserves them.

## Restored release checklists and incident evidence

- [v2.1.3.0 goals](AGENT_GOALS_2.1.3.0.md): restored from `fb42504fe^`,
  immediately before the old checklist was removed.
- [v2.1.4.0 goals](AGENT_GOALS_2.1.4.0.md): restored from `6905e3005^`,
  immediately before the new cycle replaced it. Checkbox states are snapshots.
- [Dwarfstar memory incident](BUG-005_DWARFSTAR_MEMORY_INCIDENT.md)
- [Unreferenced September screenshot](swappy-20260922-233135.png), retained
  as an unclassified historical artifact, not current operational guidance.

## October housekeeping index

- Kabuto: [codename ideas](v2.1.4.0_CODENAME_IDEAS.md),
  [security audit](v2.1.4.0_CRATE_SECURITY_AUDIT.md),
  [CI recovery](v2.1.4.0_GOAL1_CI_RECOVERY.md),
  [goal 6 follow-up](v2.1.4.0_GOAL6_REVIEW_FOLLOWUP.md),
  [EMA validation](v2.1.4.0_GOAL10_EMA_VALIDATION.md),
  [work-item audit](v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md),
  [prompt-overlay audit](v2.1.4.0_WORK_ITEM_268_PROMPT_OVERLAY_AUDIT.md).
- CI: [logic review](CICD_LOGIC_REVIEW.md), [Arch failure excerpt](CICDARCHISSUE.txt).
- Operations: [Gentoo setup](MT-GENTOO-SETUP-AND-CHANGES-MADE.md),
  [export rehearsal](IMPORT_EXPORT_EXAMPLE.txt),
  [self-repair notes](SELF_REPAIR_IMPROVEMENTS_GENTOO.md),
  [1.1.9 acceptance plan](TEST-PLAN-UPGRADED-TENANT-1.1.9.md),
  [real web acceptance](GOAL12_REAL_WEB_ACCEPTANCE.md),
  [DarkIRC flag changes](DARKIRC-MULTITENANT-CHANGES-JUN23-FLAG-INFO.md),
  [WeeChat packaging notes](WEECHAT_GENTOO_PACKAGE_RELAY_SUPPORT.md).
- Runtime/security: [REPL argument integrity](GOAL34_REPL_ARGUMENT_INTEGRITY.md),
  [sealed step output](GOAL34_SEALED_STEP_OUTPUT.md),
  [role-token smuggling](ROLE_TOKEN_SMUGGLE.md),
  [SIS fixes](SIS_FIX_PROPOSAL.md),
  [XMPP body limits](XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md),
  [NEAR removal audit](NEAR_REMOVAL_COMPLETION_AUDIT.md).
- Research: [upstream ideas](Ic_IDEAS_TAKEAWAYS.md),
  [marketplace](LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md),
  [OpenCode 2 worker exploration](opencode2extworkerideaandissuesandpotentialplan.md).
