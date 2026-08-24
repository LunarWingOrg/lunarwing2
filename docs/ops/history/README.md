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
- Older archived ops material also exists under
  [`docs/internal/history/archive/ops/`](../../internal/history/archive/ops/).

The maintained entry point is [`docs/ops/README.md`](../README.md). The active
release checklist is the `AGENT_GOALS_*.md` file kept in the parent directory;
current long-range planning lives in [`ROADMAP_2026.md`](../ROADMAP_2026.md).

When archiving another release, move its completed evidence into a versioned
subdirectory and repair repository links. Do not commit raw CI transcripts or
one-line placeholders as archival records; Git history already preserves them.
