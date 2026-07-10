---
name: codex-impl
description: Implement one bounded feature via Codex in an isolated git worktree, then verify. Use for single-feature Codex implementation or as a worker for parallel fan-out.
argument-hint: "<slug>: <feature description>"
disable-model-invocation: false
allowed-tools: Bash(git *), Bash(.claude/scripts/*), Bash(.claude/skills/codex-impl/scripts/*), Read, Grep, Glob, Edit, Write
---

# Codex single-feature implementer (Pattern A)

You are the Claude orchestrator. **Codex writes the code. You plan, constrain, dispatch, and verify.**

## Hard constraints (repo + user)

- Current main checkout is often on a protected integration branch. **Do not switch or create branches on the main checkout** unless the user explicitly allows it.
- Feature isolation uses **git worktrees** under `.worktrees/<slug>/` and branches `feat/<slug>`.
- Prefer path-scoped changes. No drive-by refactors.
- LunarWing build rules from `AGENTS.md` still apply when verifying:
  - from `ic/` use `taskset -c 0-5 cargo check -j6` (not full debug builds)
  - never use full debug `cargo build` for routine verification

## Input

$ARGUMENTS

Parse:
1. optional leading `slug:` (kebab-case)
2. remaining text = feature goal
If no slug, derive a short kebab slug from the goal.

## Procedure

### 1) Scope

Write a short plan:
- goal
- in-scope path globs (relative to repo root)
- out-of-scope paths
- acceptance checks (commands)
- files likely touched

For this monorepo, common roots include:
- `ic/src/**` core daemon
- `ic/crates/**`
- `ic/tools-src/**`, `ic/channels-src/**`
- worker containers: `lunarcode4lunarwing/**`, `pebble4lunarwing/**`, `opencode4lunarwing/**`
- bridges/tools: `xmpp_bridge/**`, `lunarwing-gotify-tool/**`, `lunarwing_weechat_wss/**`

If scope is ambiguous, ask once for path hints before calling Codex.

### 2) Worktree (isolated; does not move main checkout)

From repo root:

```bash
.claude/scripts/feature-worktree.sh "<slug>"
```

Capture `WORKTREE` and `BRANCH`.  
If the user forbade creating branches, stop and ask — worktree creation intentionally makes `feat/<slug>` for isolation.

Default base is `origin/main`/`origin/master` if present, else current `HEAD`.  
To pin base to the current integration tip without guessing:

```bash
.claude/scripts/feature-worktree.sh "<slug>" "$(git rev-parse HEAD)"
```

### 3) Prompt file for Codex

Write `$WORKTREE/.codex-prompt.md`:

```markdown
# Task
<feature goal>

# Repository
- Product: LunarWing (Rust monorepo; core in ic/)
- Follow AGENTS.md coding rules
- Work ONLY under: <in-scope globs>
- Do NOT modify: <out-of-scope>
- Match existing style; no unrelated refactors
- No new deps unless required (justify if so)

# Build constraints
- Prefer cargo check over cargo build
- If running cargo: taskset -c 0-5 ... -j6
- DO NOT use full debug builds

# Acceptance
- <check 1>
- <check 2>

# Deliverable
- Implement the change
- Minimal diff
- End with summary of files changed + how to verify
```

### 4) Run Codex

```bash
.claude/skills/codex-impl/scripts/run-codex.sh "$WORKTREE" "$WORKTREE/.codex-prompt.md"
```

Env overrides:
- `CODEX_SANDBOX` (default `workspace-write`)
- `CODEX_ASK` (default `never`)
- `CODEX_MODEL`
- `CODEX_EXTRA_ARGS`

Logs land in `$WORKTREE/.codex-runs/`.

### 5) Verify (you, not Codex)

In `$WORKTREE`:
1. `.claude/scripts/feature-status.sh "$WORKTREE"`
2. Inspect `git diff` — reject out-of-scope files (restore them)
3. Run acceptance checks scoped to the change
4. One tight Codex fixup pass only if needed

### 6) Return

```markdown
## Feature: <slug>
- Branch: ...
- Worktree: ...
- Status: success | needs-input | failed
- Diff summary: ...
- Verification: ...
- Log: ...
- Next: open PR / revise / blocked
```

Do not merge, force-push, or switch the main checkout branch.
