# Claude → Codex parallel feature orchestration

Pattern A: **Claude Code orchestrates**, **Codex implements** in isolated git worktrees.

> **Full guide:** [`docs/guides/CLAUDE_CODEX_ORCHESTRATION.md`](../docs/guides/CLAUDE_CODEX_ORCHESTRATION.md)

## Layout

```text
.claude/
  scripts/
    feature-worktree.sh   # create/reuse .worktrees/<slug> + feat/<slug>
    feature-status.sh     # status/diff summary for a worktree
  skills/
    codex-impl/           # /codex-impl  — one feature
      scripts/run-codex.sh
    features-parallel/    # /features-parallel — many features
```

## Prerequisites

- `codex` on PATH (codex-cli 0.144.x+)
- git repo (LunarWing root)
- optional: `gh` for PRs

## Usage

```text
/codex-impl my-slug: implement X only under ic/src/foo/**
```

```text
/features-parallel
feat-a: ... under ic/src/a/**;
feat-b: ... under opencode4lunarwing/**
```

## Codex invocation defaults

`run-codex.sh` uses:

```bash
codex exec \
  -C <worktree> \
  -s workspace-write \
  -c 'approval_policy="never"' \
  --ephemeral --json -o <last-msg> \
  - < prompt
```

Note: `codex exec` has no `-a` flag; approval is set via `-c approval_policy=...`.

Overrides via env: `CODEX_SANDBOX`, `CODEX_APPROVAL`, `CODEX_MODEL`, `CODEX_EXTRA_ARGS`, `CODEX_BYPASS_APPROVALS=1`.

## Branch policy

- Feature work lives on `feat/<slug>` in `.worktrees/<slug>`
- Integration branch (e.g. `sloptegration/upgrade/v2.0.0.0`) may receive orchestration tooling commits
- Do not touch `master`/`main` for feature implementation
