# Claude → Codex parallel feature orchestration

Pattern A: **Claude Code orchestrates**, **Codex implements** in isolated git worktrees.

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

- `codex` on PATH (this machine: codex-cli 0.144.1)
- git repo (LunarWing root)
- optional: `gh` for PRs

## Usage

From the **LunarWing repo root** (or a Claude session whose cwd is that root):

```text
/codex-impl my-slug: implement X only under ic/src/foo/**
```

```text
/features-parallel
feat-a: ... under ic/src/a/**;
feat-b: ... under opencode4lunarwing/**;
feat-c: ... under pebble4lunarwing/**
```

## Safety notes for this branch

- Main checkout is often pinned (e.g. `sloptegration/upgrade/v2.0.0.0`).
- Setup and normal orchestration must **not** `git checkout` other branches on the main worktree.
- Isolation branches live only under `.worktrees/` as `feat/<slug>`.
- Creating those feature branches/worktrees is intentional for parallel work; ask first if the user forbade any new branches.

## Codex invocation defaults

`run-codex.sh` uses:

```bash
codex exec -C <worktree> -s workspace-write -c 'approval_policy="never"' --ephemeral --json -o <last-msg> - < prompt
```

Overrides via env: `CODEX_SANDBOX`, `CODEX_APPROVAL`, `CODEX_MODEL`, `CODEX_EXTRA_ARGS`.

## Ignore rules

`.worktrees/` is already gitignored. Also ignore:

```gitignore
**/.codex-runs/
**/.codex-prompt.md
```
