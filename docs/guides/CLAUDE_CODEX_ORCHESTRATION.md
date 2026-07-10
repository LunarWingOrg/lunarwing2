# Claude Code → Codex Parallel Feature Orchestration

How to develop **multiple unrelated features at once** in the LunarWing monorepo by letting **Claude Code orchestrate** and **OpenAI Codex implement** inside isolated git worktrees.

This is **Pattern A**: Claude is the boss; Codex is a scoped worker process.

| Role | Tool | Responsibility |
|------|------|----------------|
| Orchestrator | Claude Code | Scope, path bounds, worktrees, dispatch, verify, scoreboard |
| Implementer | Codex CLI (`codex exec`) | Write code inside one feature worktree |
| Isolation | `git worktree` + `feat/<slug>` | Parallel features without fighting one checkout |

**Related files (in-repo):**

| Path | Purpose |
|------|---------|
| [`.claude/skills/codex-impl/`](../../.claude/skills/codex-impl/) | `/codex-impl` skill — single feature |
| [`.claude/skills/features-parallel/`](../../.claude/skills/features-parallel/) | `/features-parallel` skill — many features |
| [`.claude/scripts/feature-worktree.sh`](../../.claude/scripts/feature-worktree.sh) | Create/reuse `.worktrees/<slug>` |
| [`.claude/scripts/feature-status.sh`](../../.claude/scripts/feature-status.sh) | Status / changed-file summary |
| [`.claude/skills/codex-impl/scripts/run-codex.sh`](../../.claude/skills/codex-impl/scripts/run-codex.sh) | Non-interactive Codex wrapper |
| [`.claude/README-codex-orchestration.md`](../../.claude/README-codex-orchestration.md) | Short pointer / defaults |

Also respect root agent rules: [`AGENTS.md`](../../AGENTS.md), [`AI-CODE-CONTRIBUTION-POLICY.md`](AI-CODE-CONTRIBUTION-POLICY.md).

---

## When to use this

**Use it when:**

- You have **2+ independent features** (path-disjoint packages or subsystems)
- The monorepo is large enough that one shared dirty checkout becomes painful
- You want Claude to **plan and verify**, and Codex to **implement** bulk edits
- You are working on an integration branch (e.g. `sloptegration/upgrade/v2.0.0.0`) and must **not** land experimental work on `master` / `main`

**Do not use it when:**

- The change is a one-line fix Claude can do faster alone
- Features share the same core files (serialize or redesign scope)
- You need interactive Codex TUI pair-programming (use `codex` interactively instead of `run-codex.sh`)

---

## Mental model

```text
You
 └── Claude Code (session on integration branch)
      ├── Feature A  →  .worktrees/a  (branch feat/a)  →  codex exec
      ├── Feature B  →  .worktrees/b  (branch feat/b)  →  codex exec
      └── Feature C  →  .worktrees/c  (branch feat/c)  →  codex exec
             │
             └── Claude verifies each worktree (diff + cargo check / tests)
```

Rules of the road:

1. **Main checkout stays put.** Claude does not `git checkout` away from your integration branch for feature work.
2. **Each feature owns a worktree** under `.worktrees/<slug>/` and a branch `feat/<slug>`.
3. **Codex only writes inside its worktree**, ideally only under path globs you put in the prompt.
4. **Claude verifies** after Codex exits (scope bleed, checks, commit readiness).
5. **`master` / `main` are off-limits** for this workflow unless you explicitly say otherwise.

`.worktrees/` is gitignored. Logs under `**/.codex-runs/` and prompt files `**/.codex-prompt.md` are also ignored.

---

## Prerequisites

### Software

| Tool | Notes |
|------|--------|
| Git 2.x+ | Worktrees required |
| [Claude Code](https://code.claude.com) | Orchestrator session |
| [Codex CLI](https://github.com/openai/codex) | `codex` on `PATH` — validated with **codex-cli 0.144.x** |
| Optional: `gh` | Opening PRs from worktrees |

Check:

```bash
command -v claude
command -v codex && codex --version
codex login status    # must show logged in (API key or ChatGPT)
git rev-parse --show-toplevel
```

### Repo location

Run everything from the **LunarWing monorepo root** (the directory that contains `ic/`, `docs/`, `AGENTS.md`, and `.claude/`).

```bash
cd /path/to/LunarWing
ls .claude/skills/codex-impl .claude/scripts
```

### Claude session tips

- Start Claude with cwd = repo root.
- Prefer **acceptEdits** or Manual permission mode for this workflow. **Auto mode** can block Bash if its safety classifier is unavailable.
- Project skills load from `.claude/skills/` automatically.

### LunarWing build constraints (verification)

When verifying Rust changes, follow `AGENTS.md`:

```bash
# From ic/ — preferred compile check (NOT full debug builds)
taskset -c 0-5 cargo check -j6
```

Do **not** use full debug `cargo build` for routine verification on constrained hosts.

---

## Layout reference

```text
.claude/
  README-codex-orchestration.md
  scripts/
    feature-worktree.sh
    feature-status.sh
  skills/
    codex-impl/
      SKILL.md
      scripts/run-codex.sh
    features-parallel/
      SKILL.md
docs/guides/
  CLAUDE_CODEX_ORCHESTRATION.md   ← this guide
.worktrees/                       # created at runtime (gitignored)
  <slug>/
    .codex-prompt.md              # per-run prompt (gitignored)
    .codex-runs/                  # logs + last-message (gitignored)
    ... full tree checkout ...
```

---

## Quick start (Claude skills)

### Single feature — `/codex-impl`

In Claude Code:

```text
/codex-impl billing-retry: exponential backoff for failed charges only under ic/src/payments/**
```

Or natural language that matches the skill description:

```text
Use codex-impl for slug search-synonyms: add synonym expansion only under apps/search/**
```

What Claude should do:

1. Scope paths + acceptance checks  
2. Create worktree: `.claude/scripts/feature-worktree.sh <slug> $(git rev-parse HEAD)`  
3. Write `$WORKTREE/.codex-prompt.md`  
4. Run `.claude/skills/codex-impl/scripts/run-codex.sh "$WORKTREE" "$WORKTREE/.codex-prompt.md"`  
5. Verify with `feature-status.sh` + targeted checks  
6. Report status block  

### Multiple features — `/features-parallel`

```text
/features-parallel
billing-retry: exponential backoff under ic/src/payments/**;
search-synonyms: synonym expansion under <path>/**;
admin-csv: CSV export under <path>/**
```

Claude fans out path-disjoint workers (default cap **3–4** concurrent unless you raise it). Each worker follows `/codex-impl` semantics. Expect a **scoreboard** table at the end.

---

## Manual CLI walkthrough (no skill required)

Use this when debugging the pipeline or scripting outside Claude.

### 1. Stay on your integration branch

```bash
git branch --show-current
# e.g. sloptegration/upgrade/v2.0.0.0

# Do NOT: git checkout master
```

### 2. Create a feature worktree

```bash
# Base on current HEAD (recommended on integration branches)
.claude/scripts/feature-worktree.sh my-feature "$(git rev-parse HEAD)"
```

Example output:

```text
WORKTREE=/.../LunarWing/.worktrees/my-feature
BRANCH=feat/my-feature
STATUS=created
BASE=<sha>
```

Re-running with the same slug **reuses** the worktree.

Confirm isolation:

```bash
git worktree list
git branch --show-current                    # still integration branch
git -C .worktrees/my-feature branch --show-current   # feat/my-feature
```

### 3. Write a scoped Codex prompt

Create `.worktrees/my-feature/.codex-prompt.md`:

```markdown
# Task
Implement <one sentence goal>.

# Repository constraints
- Work ONLY under: `ic/src/foo/**`
- Do NOT modify any other paths
- Follow AGENTS.md (Rust style, no full debug builds for verification)
- No drive-by refactors; no new deps unless required and justified
- You are already on branch feat/my-feature in this worktree — do not switch branches

# Acceptance
- <test or cargo check command>
- Diff limited to the globs above

# Deliverable
- Implement the change
- Minimal diff
- End with files changed + how to verify
```

**Prompt quality is the main lever.** Tight globs beat vague “fix the monorepo” prompts.

### 4. Run Codex non-interactively

```bash
.claude/skills/codex-impl/scripts/run-codex.sh \
  .worktrees/my-feature \
  .worktrees/my-feature/.codex-prompt.md
```

Under the hood (codex-cli 0.144.x):

```bash
codex exec \
  -C <worktree> \
  -s workspace-write \
  -c 'approval_policy="never"' \
  --skip-git-repo-check \
  --ephemeral \
  --color never \
  --json \
  -o <worktree>/.codex-runs/<stamp>.last-message.md \
  - < <prompt-file>
```

Important: **`codex exec` has no `-a` flag.** Approval is set with `-c approval_policy=...`.

Artifacts:

| Path | Contents |
|------|----------|
| `.codex-runs/<stamp>.log` | Full JSONL + tee’d stdout |
| `.codex-runs/<stamp>.last-message.md` | Final agent message |

### 5. Verify (Claude or human)

```bash
.claude/scripts/feature-status.sh .worktrees/my-feature HEAD
git -C .worktrees/my-feature status -sb
git -C .worktrees/my-feature diff --stat

# Scope check: list files outside allowed globs, restore if needed
git -C .worktrees/my-feature diff --name-only HEAD

# Targeted compile (example)
cd .worktrees/my-feature/ic
taskset -c 0-5 cargo check -j6 -p <crate>
```

### 6. Commit on the feature branch only

```bash
cd .worktrees/my-feature
git add <paths>
git commit -m "feat(my-feature): <summary>"
```

Integration branch and `master` stay unchanged until you deliberately merge or open a PR.

### 7. Optional: open a PR from the worktree

```bash
cd .worktrees/my-feature
git push -u origin HEAD
gh pr create --fill --base sloptegration/upgrade/v2.0.0.0
# or --base <your integration branch>
```

Pick the base branch carefully — usually the integration branch, **not** `master`, until the upgrade is ready.

---

## Environment overrides for `run-codex.sh`

| Variable | Default | Meaning |
|----------|---------|---------|
| `CODEX_SANDBOX` | `workspace-write` | `read-only` \| `workspace-write` \| `danger-full-access` |
| `CODEX_APPROVAL` | `never` | Passed as `-c approval_policy="..."` (`never` \| `on-request` \| `untrusted`) |
| `CODEX_MODEL` | (Codex default) | `-m <model>` |
| `CODEX_EXTRA_ARGS` | empty | Extra raw args (word-split) |
| `CODEX_BYPASS_APPROVALS` | `0` | Set `1` to add `--dangerously-bypass-approvals-and-sandbox` (**dangerous**) |

Examples:

```bash
CODEX_MODEL=o3 \
CODEX_SANDBOX=workspace-write \
  .claude/skills/codex-impl/scripts/run-codex.sh "$WT" "$WT/.codex-prompt.md"

# Stronger autonomy only inside an already-sandboxed environment
CODEX_BYPASS_APPROVALS=1 \
  .claude/skills/codex-impl/scripts/run-codex.sh "$WT" "$WT/.codex-prompt.md"
```

---

## Parallel fan-out recipe

### Good parallelism

Features that touch **different trees**:

| Feature | Example globs |
|---------|----------------|
| A | `ic/src/channels/web/**` |
| B | `opencode4lunarwing/**` |
| C | `pebble4lunarwing/**` |
| D | `docs/guides/**` only |

### Bad parallelism (serialize instead)

- Two features both rewriting `ic/src/agent/mod.rs`
- Schema migration + code that depends on the new schema mid-flight
- Anything that requires a single shared lockfile edit unless coordinated

### Suggested concurrency

| Host | Concurrent Codex workers |
|------|---------------------------|
| Laptop / small VM | 2 |
| Dev box (this repo’s Gentoo-style constraints) | 3–4 |
| Large machine + high API limits | 4–6, watch disk (each worktree is a full checkout) |

Disk note: each worktree is a full working tree. On large monorepos, prune finished worktrees:

```bash
git worktree remove .worktrees/my-feature
# or: git worktree remove --force .worktrees/my-feature
git branch -d feat/my-feature   # after merge, if desired
```

### Path cheat-sheet (LunarWing)

Verify before locking into a prompt:

| Area | Typical paths |
|------|----------------|
| Core daemon | `ic/src/agent/**`, `ic/src/tools/**`, `ic/src/channels/**` |
| Engine crates | `ic/crates/**` |
| WASM tools / channels | `ic/tools-src/**`, `ic/channels-src/**` |
| Workers | `lunarcode4lunarwing/**`, `pebble4lunarwing/**`, `opencode4lunarwing/**` |
| MT onboard / health | `lunarwing_mt_onboard/**`, `ic-infrastructure-health-check/**` |
| Bridges | `xmpp_bridge/**`, `lunarwing_weechat_wss/**`, `lunarwing-gotify-tool/**` |

---

## Branch policy (recommended for upgrade lines)

| Branch | Allowed actions |
|--------|-----------------|
| `sloptegration/upgrade/v2.0.0.0` (or your integration branch) | Tooling commits, merges from verified feature branches, docs |
| `feat/<slug>` worktrees | Feature implementation commits |
| `master` / `main` | **Do not touch** via this workflow |

`feature-worktree.sh` defaults base to `origin/main` / `origin/master` if present when no base arg is given. On upgrade branches, **always pass an explicit base**:

```bash
.claude/scripts/feature-worktree.sh my-feature "$(git rev-parse HEAD)"
# or
.claude/scripts/feature-worktree.sh my-feature origin/sloptegration/upgrade/v2.0.0.0
```

---

## Validated end-to-end demo

On branch `sloptegration/upgrade/v2.0.0.0` this pipeline was validated as follows:

1. Committed orchestration skills/scripts on the integration branch  
2. Created worktree `.worktrees/codex-demo-hello` → branch `feat/codex-demo-hello`  
3. Ran `run-codex.sh` with a prompt scoped to `docs/codex-orchestration-demo/**`  
4. Codex created:
   - `docs/codex-orchestration-demo/HELLO.md`
   - `docs/codex-orchestration-demo/checksum.txt` (`codex-demo-ok`)
5. Committed demo on `feat/codex-demo-hello` only  
6. Main checkout remained on `sloptegration/upgrade/v2.0.0.0`; **master unchanged**

Re-run a similar smoke test any time:

```bash
.claude/scripts/feature-worktree.sh codex-demo-hello "$(git rev-parse HEAD)"
# write a tiny docs-only prompt, then:
.claude/skills/codex-impl/scripts/run-codex.sh \
  .worktrees/codex-demo-hello \
  .worktrees/codex-demo-hello/.codex-prompt.md
```

---

## Troubleshooting

### `error: unexpected argument '-a'`

Old wrapper assumed `codex exec -a`. **0.144.x has no `-a`.** Use current `run-codex.sh` (`-c approval_policy=...`).

### Codex exits immediately / auth errors

```bash
codex login status
codex doctor
```

Log in with an API key or ChatGPT auth before unattended runs.

### Codex did nothing / asked for approval

Ensure `CODEX_APPROVAL=never` (default) or set `CODEX_BYPASS_APPROVALS=1` only in a trusted outer sandbox. Check the JSONL log under `.codex-runs/`.

### Scope bleed (files outside globs)

```bash
git -C .worktrees/<slug> diff --name-only
git -C .worktrees/<slug> checkout -- <out-of-scope-path>
# tighten prompt, one fixup Codex pass
```

### Worktree already exists / wrong branch

```bash
.claude/scripts/feature-status.sh .worktrees/<slug>
git worktree list
# remove and recreate if corrupted:
git worktree remove .worktrees/<slug>
.claude/scripts/feature-worktree.sh <slug> "$(git rev-parse HEAD)"
```

### Main checkout accidentally dirty from feature files

Feature files should only appear under `.worktrees/`. If you edited the main tree by mistake, don’t mix commits — keep feature commits in the worktree.

### Claude Auto mode blocks Bash

If you see classifier errors like “model temporarily unavailable… auto mode cannot determine safety”, switch permission mode with **Shift+Tab** to Manual or acceptEdits, or restart with `--permission-mode acceptEdits`.

### Disk full

Each worktree is large. Remove finished ones promptly; avoid unbounded parallel fan-out.

### Cargo builds melt the machine

Use `taskset -c 0-5 cargo check -j6` per `AGENTS.md`. Never default to full debug builds for verification.

---

## Security notes

- `approval_policy=never` + `workspace-write` still confines Codex’s shell writes to the worktree sandbox policy, but **prompts can still ask the model to do harmful things inside that tree**. Keep prompts scoped.
- `CODEX_BYPASS_APPROVALS=1` disables Codex’s own approval and sandbox — only for environments that are already isolated.
- Do not put secrets into `.codex-prompt.md` (even though it is gitignored, logs may capture it).
- Review feature-branch diffs before merging into the integration line.
- Follow [`AI-CODE-CONTRIBUTION-POLICY.md`](AI-CODE-CONTRIBUTION-POLICY.md) for AI-authored contributions.

---

## Day-two operations

### List active feature work

```bash
git worktree list
ls .worktrees
for d in .worktrees/*; do
  [ -d "$d" ] || continue
  echo "== $d =="
  .claude/scripts/feature-status.sh "$d" HEAD | head -20
done
```

### Merge a finished feature into the integration branch

```bash
# From main checkout on integration branch:
git merge --no-ff feat/my-feature
# or open a PR targeting the integration branch
```

### Abandon a feature

```bash
git worktree remove .worktrees/my-feature
git branch -D feat/my-feature
```

### Update orchestration tooling

Edit files under `.claude/` on the integration branch, commit there, then either:

- recreate worktrees from the new HEAD, or  
- merge/rebase feature branches onto the updated integration tip when needed  

---

## FAQ

**Q: Can Claude implement without Codex?**  
Yes. This stack is optional. Use Codex when you want a second model/tooling path or bulk implementation fan-out.

**Q: Can Codex orchestrate Claude instead?**  
That’s Pattern B (not this guide). You’d call `claude -p ...` from Codex. Here Claude remains the orchestrator.

**Q: Does this require ultracode / dynamic workflows?**  
No. Skills + shell scripts are enough. Ultracode can wrap the same steps in a workflow if you want automatic multi-agent fan-out.

**Q: Where do I put long-lived project conventions for Codex?**  
Keep shared rules in `AGENTS.md` / path-scoped docs. Put per-run constraints in `.codex-prompt.md`.

**Q: Why not one worktree for everything?**  
Parallel unrelated features on one dirty tree thrash each other. Worktrees give each feature a branch and a clean place to fail.

---

## Checklist (copy/paste)

```text
[ ] On integration branch (not master)
[ ] codex login status OK
[ ] Feature list is path-disjoint
[ ] For each feature:
    [ ] feature-worktree.sh <slug> $(git rev-parse HEAD)
    [ ] .codex-prompt.md with tight globs + acceptance checks
    [ ] run-codex.sh
    [ ] feature-status.sh + scope review
    [ ] targeted cargo check / tests
    [ ] commit on feat/<slug>
[ ] Scoreboard reviewed
[ ] PRs or merges targeted at integration branch
[ ] Finished worktrees removed when done
```

---

## See also

- [`.claude/README-codex-orchestration.md`](../../.claude/README-codex-orchestration.md) — short defaults  
- [`AGENTS.md`](../../AGENTS.md) — agent coding contract  
- [`TESTING_GUIDE.md`](TESTING_GUIDE.md) — pre-release testing  
- [`AI-CODE-CONTRIBUTION-POLICY.md`](AI-CODE-CONTRIBUTION-POLICY.md) — AI contribution policy  
- [`ENABLING_DEV_TOOLS.md`](ENABLING_DEV_TOOLS.md) — tenant-side dev tools (product runtime; separate from this host-side orchestration)  
