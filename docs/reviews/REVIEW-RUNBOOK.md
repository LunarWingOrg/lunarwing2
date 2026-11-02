# Open-MR adversarial review: runbook

This is the harness-agnostic description of the recurring review loop for `lunarwing/lunarwing2`. Any agent (Claude Code, opencode, kimi, a cron job, a person) should be able to run one iteration from this file, the two tools in `docs/reviews/tools/`, a shell, git and a GitLab token. Nothing here depends on a particular model, MCP server, scratchpad or memory system.

The loop was run 310 times between 2026-10-03 and 2026-11-02 before this runbook was written; the rules below are the ones that survived.

## 1. What a run is

One run = one bounded pass over the open merge requests that target the current release branch, with:

- a full adversarial review and one posted GitLab note for every MR that is **new** or whose **head moved** since its last review;
- a one-line "skipped" row for every MR whose head is unchanged (no re-review, no note, even if the target moved);
- a 20-minute watch for pushes that arrive during the run;
- a report file and an updated state file at the end.

Scope: non-draft MRs whose `target_branch` is the review target (`aragents/kimil00p/v2.1.8.0` since run 286). MRs against `master` are out of scope: SweetieBot's `sweetiebot/test-*` inttest MRs, the maintainer's `wip:` branches, the old drafts. The target moves at each release (`v2.1.5.0` → `v2.1.6.0` → `v2.1.7.0` → `v2.1.8.0`); the review prefix sequence continues across releases.

The deliverable the maintainer reads is the GitLab note. The report and state file are for the next run.

## 2. Files

| Path | Tracked? | Purpose |
|---|---|---|
| `docs/reviews/open-mr-review-state.json` | no (never commit) | per-MR reviewed head, prefix, verdict, note ids; per-run entries; `last_run`, `next_prefix`, `target_heads` |
| `docs/reviews/OPEN-MR-ADVERSARIAL-REVIEW-<date>-run<N>.md` | no | the run report |
| `docs/reviews/tools/review.py` | yes | GitLab + state-file helper (`status`, `jobs`, `post`, `note-id`, `merge-state`, `prefix`) |
| `docs/reviews/tools/watch-refs.sh` | yes | bounded ref watcher |
| `<scratch>/run<N>/` | no | per-run working set: `baseline.txt`, `watch.log`, `diff<iid>.patch`, `note<iid>.md`, `fragment.json` |

The state file is **merged, never overwritten** (another session may have written it). The shape that matters:

```
last_run, last_run_at, report, next_prefix          scalars
target_heads: {"run309": "cffe5377c", ...}
mrs: {"<iid>": {title, author, source, target, head_sha, prefix, posted_run,
                posted_note_id, posted_note_ids, posted_at, verdict, findings, ci}}
runs: [{run, start, end, target, master, window, mrs_reviewed, mrs_skipped,
        notes_posted, prefixes_used, verdicts, ci, merge_order, ...}]
```

Scratch: use the harness scratchpad if it has one, else `/var/lib/paseo/.paseo/tmp/run<N>`. Delete exported trees, tarballs and cargo targets at the end of the run (the operator wants `.paseo/tmp` small); keep the small files (notes, fragment, baseline) until the state file is merged.

## 3. Prerequisites

- A checkout of the repo. `/var/lib/paseo/wt` is the operator's shared worktree: it is usually on the target branch and may be dirty. **Never check out an MR head in it and never revert anything in it.** Read MR heads with `git show <sha>:<path>`, `git diff <base> <sha>`, `git grep -n <pat> <sha> -- <path>`, or `git archive <sha> | tar -x -C <scratch>`.
- A GitLab token with `api` scope, in `GITLAB_TOKEN` or in a file named by `GITLAB_TOKEN_FILE`. Let the tool read the file; never `cat` the token into a command line (some harnesses block that, and it lands in logs).
- `git`, `bash`, `python3` (stdlib only), `curl` is not needed.
- Cargo rules of the host (`AGENTS.md`): `taskset -c 0-11`, `-j12`, `cargo check` and targeted `cargo test` only, no debug builds, never build under `/tmp`, **one cargo process on the host at a time**. Most reviews need no cargo at all; CI's job logs are the compile evidence.

## 4. A run, step by step

### 4.1 Open

```bash
cd /var/lib/paseo/wt
N=$(( $(python3 -c "import json;print(json.load(open('docs/reviews/open-mr-review-state.json'))['last_run'])") + 1 ))
S=<scratch>/run$N && mkdir -p "$S"
date -u +%FT%TZ | tee "$S/start.txt"                       # run start; the window is 20 min from here
git fetch -q origin && git ls-remote origin refs/heads/aragents/kimil00p/v2.1.8.0 refs/heads/master
docs/reviews/tools/review.py status                        # NEW / CHANGED / UNCHANGED per open MR
docs/reviews/tools/watch-refs.sh -m 20 "$S/baseline.txt" > "$S/watch.log" 2>&1 &   # or in tmux
```

Record the run start, the target and master heads, the baseline time and ref count (first line of `watch.log`), and the to-review list. In Claude Code the watcher runs as a background Bash; elsewhere use `tmux new-session -d` or `nohup`. Its exit code tells the story: 0 = change (the moved refs are printed), 124 = quiet window.

If a previous run left posting-ready notes or an unmerged fragment (see §7), finish that **before** opening the new run.

### 4.2 Review each NEW or CHANGED MR

One two-letter prefix per MR per review (`review.py prefix 3` shows the next three; they are consumed when the fragment is merged). Findings are numbered `<prefix>-<n>`.

For every MR:

1. **Context.** `git fetch origin refs/merge-requests/<iid>/head` (or the source branch), `git merge-base origin/<target> <head>`, export `git diff <mb> <head> > $S/diff<iid>.patch`, read the commit list. "Item #N" MRs implement one item of `docs/ops/AGENT_GOALS_<ver>.md` and usually one work item (`GET /issues/<wi>`): read both, and check the **lane rule** (the item names the agent that owns it: applejack, sweetiebot, rares/rarity, …; the author must match).
2. **Re-attempts.** If the MR re-implements a closed MR, find the closed MR's review note and disposition every prior finding ID: FIXED / PARTIAL / UNADDRESSED, each with `file:line`. Compare per commit with `git patch-id --stable` when the trail claims a replay.
3. **Read the whole diff**, then the surrounding code at the head. Trace trust boundaries end to end (config precedence, first-run paths, auth gates before side effects, lock ordering, generation/ownership keys). Severity scale: Blocker, High, Medium, Low, Info.
4. **Verify claims independently.** Pins via `git ls-remote <upstream>`; lock-file integrity against the registry; CI claims via the job log, not the trail; "every pipeline since X is green" via the pipelines list; conflicts via `git merge-tree --write-tree --merge-base=<target> <a> <b>` pairwise with every other open MR (also catches real code conflicts).
5. **Re-verify before posting** every High or Blocker claim and every "this defect does not exist" claim against the tree, with `file:line`. If a subagent did the reading, re-verify its claims yourself; findings without a file:line anchor do not get posted.
6. **CI.** `review.py jobs <pipeline>` for the head pipeline; read the log of every failed job. Known standing reds are named as such (e.g. a RustSec advisory that reds `satellite-advisory-sweep` on every pipeline after its publication time) and never attributed to the MR.
7. **Draft the note** as `$S/note<iid>.md` in the format of §5, with `date -u` taken right before posting for any timestamp in it.

What no review does: check out the head in the shared worktree; run a full debug build; trust a goals-trail "verified" claim without the artifact it names; edit `AGENT_GOALS_*.md` (only when the operator explicitly asks for a goals edit).

### 4.3 Post

```bash
date -u +%FT%TZ
docs/reviews/tools/review.py post <iid> "$S/note<iid>.md" --head <sha-reviewed> --record
```

`--head` refuses to post if the MR moved since the review (agent pairs often push a fix minutes after opening; re-review the new head instead). `--record` writes the note id into the state file at once, so a crashed session loses nothing. The GitLab approve button is **not** clicked; the verdict lives in the note text. When a pipeline was still running at posting time and the result matters, post a short **CI addendum** note once it finishes (same prefix, `--any-head`), and record it with `--record` too.

### 4.4 Watcher alerts

When `watch-refs.sh` exits 0 inside the window:

- a new `refs/merge-requests/<iid>/head` or a moved one: run `review.py status`, review it (§4.2), post (§4.3);
- a branch-only change: still re-run `status`; the MR ref can lag the branch push and hide in the new baseline;
- out-of-scope changes (inttest MRs, maintainer wip branches): note them in the report, no review.

Then re-arm the watcher with a **fresh baseline** for the remainder of the window (`-m <minutes left>`). The window is 20 minutes from the run start; it is extended only when the operator asks, once per run; a finished run is never reopened. Stop the watcher (by PID or task id) at the end of the run if it is still polling.

### 4.5 Close

1. Final `review.py status` (heads can move while the report is being written).
2. CI on the target and master: latest pipeline on each, the tenant build branch `testing/t/build/<n>/tenants/<ver>` if one exists, standing advisory reds, open CI work items.
3. Write the report (§6) to `docs/reviews/OPEN-MR-ADVERSARIAL-REVIEW-<date>-run<N>.md`.
4. Write the fragment `$S/fragment.json` (§7) and merge it: `review.py merge-state $S/fragment.json` (`--dry-run` first).
5. Delete the heavy scratch; keep the notes and fragment until the merge is confirmed.
6. Tell the operator: what was reviewed, verdicts and note ids, what was skipped, the merge order, and anything unusual (a tool that failed, a window that was unwatched).

## 5. Note format

```
## Adversarial review (run <N>, prefix <PP>) — head `<sha9>`, target `<sha9>`

**Verdict: APPROVE | APPROVE WITH NOTES | APPROVE WITH NITS | REQUEST CHANGES (narrow) for PP-1 | REQUEST CHANGES | BLOCK.**
One paragraph: the one-line reason, the CI state, the merge condition.

Scope: commits, files, +/-; what the MR does; lane rule; merge-tree result against the target and every open sibling.

### Verified            (for re-attempts: disposition table of every prior finding ID first)
- bullets with file:line anchors

### Findings
**PP-1 (High): title.** evidence with file:line, consequence, concrete fix.
**PP-2 (Info): …**

**Security:** one line on listeners, auth, secrets, sandboxing, approvals, outbound HTTP (or "no change").
**Merge readiness:** condition, in one line.
```

CI addendum: `**CI addendum (run N, prefix PP) for head \`sha9\`.** pipeline, counts, the red jobs and whose they are, whether the verdict changes.`

Severities are strict by request of the operator: a Blocker is something that must not merge; High is a real defect on a reachable path; Medium is a defect with a workaround or a narrow path; Low is a carried risk or a cosmetic defect in a security-relevant place; Info is everything the author should know but need not act on. When a later run finds an earlier finding wrong, withdraw it explicitly in the new note.

## 6. Report format

Header line with run number, times, target and master heads. Then: scope paragraph (what changed since the last run, what is out of scope), next prefix, a summary table (MR, item/WI, author and branch, head and whether it changed, verdict with run and note ids, merge status, pipeline), one section per reviewed MR (scope, verified, disposition, findings, verdict, note id, time), one short section per skipped MR (head, standing verdict, pipeline change since), **merge order** (sequential, with conditions), CI on the target and master, and the window (start, baseline time and count, alerts, exit code, final snapshot). Run 308's and 310's reports are good small examples; run 309's is a large one.

## 7. State fragment

The run's state changes are written to one JSON file and merged with `review.py merge-state`, which refuses a double merge (`last_run` must be exactly one more than the state's) and unions `posted_note_ids` with ids already recorded by `post --record`.

```json
{
  "last_run": 310, "last_run_at": "2026-11-02T15:25Z",
  "report": "docs/reviews/OPEN-MR-ADVERSARIAL-REVIEW-2026-11-02-run310.md",
  "next_prefix": "UR", "target_head": "cffe5377c",
  "mrs": {
    "2162": {"title": "…", "author": "…", "source": "…", "target": "aragents/kimil00p/v2.1.8.0",
             "head_sha": "<full sha>", "prefix": "UR", "posted_run": 310,
             "verdict": "APPROVE (UR-1 Info …)", "findings": "…", "ci": "7100 34/34"}
  },
  "runs_entry": {"run": 310, "start": "…", "end": "…", "target": "… cffe5377c", "master": "f2174d0d8",
                 "window": "…", "mrs_reviewed": ["2162"], "mrs_skipped": ["2155"], "notes_posted": {"2162": [13660]},
                 "prefixes_used": {"2162": "UR"}, "verdicts": {"2162": "APPROVE"}, "ci": "…", "merge_order": ["…"]}
}
```

Keys starting with `_` are ignored, so a `_howto` note for the next session is fine. If a session dies before the merge, the next session merges the fragment first (§4.1).

## 8. Standing positions of the maintainer

These were stated by the maintainer in review threads or to the operator; they decide severities and are not re-litigated per run.

- Release target branches are intentionally unprotected; direct agent commits on them are expected (not a finding).
- No RSA SSH keys are in use: RSA/Marvin-class advisories are Low or Info, never a blocker.
- Tenants are PostgreSQL; the libSQL remote-replica path is unused (findings there are Low/Info).
- Workers run under rootless podman; "docker group is root-equivalent" warnings are not required (WI#389).
- No worker container runs as root (2026-10-21); the split-container design replaced the root-bridge lanes.
- Workers must never self-expose (WI#357); only the egress allowlist is a boundary against a compromised agent (details in `AGENTS.md`).
- Root builds into the admin image store with save/load distribution are by design; only tenant-authored build inputs are a finding.
- opencode v1 stays (2026-11-01); do not review toward retiring it.
- `mt-admin` is root-only by design; root cron must never source operator-checkout paths.
- Commits authored "Paseo Lunarwing" are the operator's own agents; provenance is not a finding.
- The standing "latest master pipeline" goals item changes state at the maintainer's hand; its MRs are the maintainer's.
- The opencode2 candidate lane is hands-off; a CI fix there is proven only by a green protected pipeline on master.

## 9. Hazards that cost runs

- **Harness classifiers.** Claude Code's auto mode has a content-triggered safety check: in run 309 it blocked Bash, then the GitLab MCP (reads too), then Edit, in three sessions in a row, each time after reviewing the opencode worker exposure/tunnel material. It is session-wide and permanent while auto mode is on; the identical post went through the moment auto mode was turned off. So: do the Bash and GitLab work early in a run, keep every draft on disk, write the fragment as you go, and when it fires ask the operator to turn auto mode off rather than starting a fresh session (which re-trips as soon as it re-reads the material).
- **Two sessions on one run.** Check `ls -la docs/reviews/` timestamps before writing, use the run number in every filename, merge the state file.
- **Heads move right after an MR opens.** Agent pairs push fixes within minutes; `post --head` exists for this.
- **Trails overclaim.** "Verified", "all green", "carries upstream commit X" are checked, not copied: in run 309 the "carries `61845b0`" claim was a content-equivalent backport, not an ancestor.
- **Cold cargo.** A full test run on a cold target dir takes 30-40 minutes and competes with the operator's builds; CI's logs plus targeted read-only gates (`check_no_panics.py`, boundaries grep, `rustfmt --check` on an exported module) usually suffice.
- **Credential files.** The MCP token file is the harness's; give scripts their own `GITLAB_TOKEN_FILE` and never echo it.

## 10. Tool reference

```
review.py status [--json]                  open MRs vs state: NEW / CHANGED / UNCHANGED, merge status, head pipeline
review.py jobs <pipeline>                  status counts + every non-success job
review.py post <iid> <file> --head <sha> [--record] [--dry-run]   new discussion thread; refuses a moved head
review.py post <iid> <file> --any-head     e.g. a CI addendum
review.py note-id <iid> <note_id> [--at T] [--head SHA]           record an id posted by other means
review.py merge-state <fragment> [--dry-run] [--force]
review.py prefix [N]
watch-refs.sh [-m MIN] [-i SEC] [-r REPO] <baseline>              exit 0 change / 124 quiet / 2 error
```

Environment: `GITLAB_TOKEN` or `GITLAB_TOKEN_FILE`; optional `GITLAB_URL` (default `https://git.lunarwing.org`), `GITLAB_PROJECT` (default `lunarwing/lunarwing2`), `REVIEW_TARGET` (default the current target branch; also `--target`), `WATCH_IGNORE_REGEX`.
