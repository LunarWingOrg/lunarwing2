# Open-MR adversarial review: runbook

This is the harness-agnostic description of the recurring review loop for `lunarwing/lunarwing2`. Any agent (a Claude Code, opencode or kimi session, a cron job, a person) should be able to run one iteration from this file, the two tools in `docs/reviews/tools/`, a shell, git and a GitLab token. Nothing here depends on a particular model, MCP server, scratch directory or memory system; everything a run needs to know from earlier runs is in the state file and the reports.

The loop was run 334 times between 2026-10-03 and 2026-11-05 before this version of the runbook was written; the rules below are the ones that survived.

## 0. Conventions used below

The shell snippets use four variables. Set them once per session; the defaults are the operator's host.

| Variable | Default | Meaning |
|---|---|---|
| `REVIEW_CHECKOUT` | `/var/lib/paseo/wt` | the shared checkout (usually on the target branch, often dirty; read-only for a review) |
| `REVIEW_SCRATCH` | `/var/lib/paseo/.paseo/tmp` | where per-run working files go (`$REVIEW_SCRATCH/run<N>/`); any private, writable directory works |
| `REVIEW_TARGET` | `aragents/kimil00p/v2.1.8.0` | the release branch under review (also `review.py --target`) |
| `GITLAB_TOKEN_FILE` | (none) | a file holding an `api`-scope token (`GITLAB_TOKEN` in the environment also works) |

"Background" means whatever the harness offers: a background job of the harness, `tmux new-session -d`, or `nohup … &`. Record the PID of anything started in the background so it can be stopped by PID. "The operator" is the maintainer who runs the agent; "the agents" are the autonomous authors of the MRs (Applejack, Rarity, SweetieBot).

## 1. What a run is

One run = one bounded pass over the open merge requests that target the current release branch, with:

- a full adversarial review and one posted GitLab note for every MR that is **new** or whose **head moved** since its last review;
- a one-line "skipped" row for every MR whose head is unchanged (no re-review, no note, even if the target moved);
- a 20-minute watch for pushes that arrive during the run;
- a report file and an updated state file at the end.

Scope: non-draft MRs whose `target_branch` is the review target. MRs against `master` are out of scope: SweetieBot's `sweetiebot/test-*` inttest MRs, the maintainer's `wip:` branches, the old drafts. The target moves at each release (`v2.1.5.0` → `v2.1.6.0` → `v2.1.7.0` → `v2.1.8.0`); the review prefix sequence continues across releases.

The deliverable the maintainer reads is the GitLab note. The report and state file are for the next run, whoever runs it.

## 2. Files

| Path | Tracked? | Purpose |
|---|---|---|
| `docs/reviews/open-mr-review-state.json` | no (never commit) | the state file (§7): per-MR reviewed head, prefix, verdict, note ids; per-run entries; `last_run`, `next_prefix`, `target_heads` |
| `docs/reviews/OPEN-MR-ADVERSARIAL-REVIEW-<date>-run<N>.md` | no | the run report (§6) |
| `docs/reviews/tools/review.py` | yes | GitLab + state-file helper (`status`, `jobs`, `post`, `note-id`, `merge-state`, `prefix`) |
| `docs/reviews/tools/watch-refs.sh` | yes | bounded ref watcher |
| `$REVIEW_SCRATCH/run<N>/` | no | per-run working set: `start.txt`, `baseline.txt`, `watch.log`, `diff<iid>.patch`, `note<iid>.md`, `fragment.json` |

`review.py` finds the state file next to itself (`docs/reviews/open-mr-review-state.json`); the global option `review.py --state <path> <subcommand> …` overrides it (global options go before the subcommand). The state file is **merged, never overwritten**: another session may have written it since you read it. Delete exported trees, tarballs and cargo targets at the end of the run (the operator wants the scratch area small); keep the small files (notes, fragment, baseline) until the state file is merged.

## 3. Prerequisites

- A checkout of the repo at `$REVIEW_CHECKOUT`. It is the operator's: **never check out an MR head in it, never `stash`, `reset` or revert anything in it, and `git pull` only when the operator says so.** Read MR heads with `git show <sha>:<path>`, `git diff <base> <sha>`, `git grep -n <pat> <sha> -- <path>`, or `git archive <sha> <paths> | tar -x -C <scratch>` (an export; it has no `.git`, see §9).
- A GitLab token with `api` scope, in `GITLAB_TOKEN` or in a file named by `GITLAB_TOKEN_FILE`. Let the tool read the file; never `cat` the token into a command line (some harnesses block that, and it lands in logs). If the harness has its own token file for an MCP server, give the scripts their own copy under another name.
- `git`, `bash`, `python3` (stdlib only). `curl` is not needed. `bun` is not installed on the operator's host, so TypeScript suites are verified through CI only.
- Cargo rules of the host (`AGENTS.md`): `taskset -c 0-11`, `-j12`, `cargo check` and targeted `cargo test` only, no debug builds, never build under `/tmp`, **one cargo process on the host at a time** (the operator builds too). Most reviews need no cargo at all; CI's job logs are the compile evidence.

## 4. A run, step by step

### 4.1 Open

```bash
cd "${REVIEW_CHECKOUT:-/var/lib/paseo/wt}"
N=$(( $(python3 -c "import json;print(json.load(open('docs/reviews/open-mr-review-state.json'))['last_run'])") + 1 ))
S="${REVIEW_SCRATCH:-/var/lib/paseo/.paseo/tmp}/run$N" && mkdir -p "$S"
date -u +%FT%TZ | tee "$S/start.txt"                       # run start; the window is 20 min from here
git fetch -q origin && git ls-remote origin "refs/heads/${REVIEW_TARGET:-aragents/kimil00p/v2.1.8.0}" refs/heads/master
docs/reviews/tools/review.py status                        # NEW / CHANGED / UNCHANGED per open MR
docs/reviews/tools/watch-refs.sh -m 20 "$S/baseline.txt" > "$S/watch.log" 2>&1 &   # in the background; keep the PID
```

Record the run start, the target and master heads, the baseline time and ref count (first line of `watch.log`), and the to-review list. The watcher's exit code tells the story: 0 = change (the moved refs are printed), 124 = quiet window, 2 = error. Since the watcher's polls happen in the background, do not sleep waiting for it: review, draft and verify meanwhile, and read `watch.log` when the harness reports the process finished (or when the window's end time has passed).

If a previous run left posting-ready notes or an unmerged fragment (§7), finish that **before** opening the new run. If a CI addendum was deferred (a pipeline still running at the last close), post it first (§4.3).

### 4.2 Review each NEW or CHANGED MR

One two-letter prefix per MR **per review** (`review.py prefix 3` shows the next three; they are consumed when the fragment is merged). A re-review of a moved head is a new review with the next prefix. Findings are numbered `<prefix>-<n>`.

For every MR:

1. **Context.** `git fetch origin refs/merge-requests/<iid>/head:refs/mr/<iid>` (or the source branch), `git merge-base origin/<target> <head>`, export `git diff <mb> <head> > $S/diff<iid>.patch`, read the commit list. "Item #N" MRs implement one item of `docs/ops/AGENT_GOALS_<ver>.md` and usually one work item (`GET /issues/<wi>`): read both, and check the **lane rule** (item number mod 3: 1 = Rarity, 2 = Applejack, 0 = SweetieBot; the author must match) and the trail's credit line (a trail that credits the wrong agent is an Info finding).
2. **Re-attempts and moved heads.** If the MR re-implements a closed MR, find the closed MR's review note and disposition every prior finding ID: FIXED / PARTIAL / UNADDRESSED, each with `file:line`. Scope the work with a head-to-head diff: `git diff <old-mr-head> <new-mr-head> -- ic/ docs/` lists exactly the files that changed since the reviewed code; everything else is byte-identical to what was already verified and needs only a light re-check. Compare per commit with `git patch-id --stable` when the trail claims a replay. The same applies to a head that moved during a run: review the delta since the reviewed head, then disposition the previous prefix's findings.
3. **Read the whole diff**, then the surrounding code at the head. Trace trust boundaries end to end (config precedence, first-run paths, auth gates before side effects, lock ordering, generation/ownership keys). When a change claims behaviour of another component ("the TUI replaces the text", "XMPP never sees this"), read that component at the head: every `match` on the changed enum, every consumer of the changed event, the client's reducer. Severity scale: Blocker, High, Medium, Low, Info.
4. **Verify claims independently.** Pins via `git ls-remote <upstream>` and the registry's referrers API for signatures (a Sigstore bundle's certificate names the signing workflow; no cosign needed); lock-file integrity against the registry; CI claims via the job log, not the trail; "every pipeline since X is green" via the pipelines list; conflicts via `git merge-tree --write-tree --merge-base=<target> <a> <b>` pairwise with every other open MR (goals-file-only conflicts are trivial rebases; say so in the merge order). A suggested regex or classifier is tested against a real job log before it is posted (BuildKit escapes the inner quotes of a `RUN` command as `\"` in its trace; a naive `[^"]*` never matches).
5. **Re-verify before posting** every High or Blocker claim and every "this defect does not exist" claim against the tree, with `file:line`. If a subagent did the reading, re-verify its claims yourself; findings without a file:line anchor do not get posted.
6. **CI.** `review.py jobs <pipeline>` for the head pipeline; it prints only the non-success jobs and truncates the list, so when its counts exceed its rows, list the jobs through the API (`/pipelines/<id>/jobs?per_page=100&include_retried=true`) and read the log of **every** failed job. Infrastructure failures are named as such and never attributed to the MR: `snapshot.debian.org` apt stalls (a worker-image `RUN` killed with "exit code: 137" or "124"), GitHub release downloads answering 504 (`curl: (22)` right after `$ bash .gitlab/scripts/bootstrap-sccache.sh`, which fails three jobs at once and skips the tests behind `rust-check-libsql`), a RustSec advisory that reds `satellite-advisory-sweep` on every pipeline after its publication time. The check that settles it: the same job passed on the target's and master's latest pipelines, and the MR did not touch the relevant inputs. Suggest a retry; retrying is the operator's click.
7. **Recurring checks** that have produced findings more than once: a new tenant env knob needs a `_env_existing` preservation line in `ic/scripts/mt-admin-lib/06-env-generation.sh` plus lines in `docs/ops/TENANT-CONFIGURATION.md` and `ic/.env.example` (the renderer regenerates `lunarwing.env` and drops unlisted keys); every relative link and `#anchor` in changed Markdown resolves at the head; release-notes claims are checked against the merged tree **and** against the approved-but-unmerged MRs (a "known limitation" that an approved MR removes must be sequenced after that MR); a goals trail that ran cargo with more than `taskset -c 0-11 -j12` is an Info finding (`AGENTS.md`).
8. **Draft the note** as `$S/note<iid>.md` in the format of §5, with `date -u` taken right before posting for any timestamp in it.

What no review does: check out the head in the shared checkout; run a full debug build; trust a goals-trail "verified" claim without the artifact it names; edit `AGENT_GOALS_*.md` (only when the operator explicitly asks for a goals edit); click the GitLab approve button; retry a pipeline; close or merge anything.

Local verification that is cheap and safe: `python3 -s -m unittest discover -s .gitlab/tests -p '<file>.py'` from an export (six `test_main_*` cases of `test_goal1_ci.py` need a real git checkout and fail in any export, on the target too); mt-admin harnesses (`ic/scripts/tests/test-*.sh`) from an export of `ic/scripts` **and** `ic-infrastructure-health-check` (the OpenRC library sources `$HEALTH_SRC_DIR/openrc-proxy-unit-contract.sh`); `bash -n`, `shellcheck -S warning`, `python3 -m py_compile`, `validate_config.py`, `verify_release_version.py --self-test`, `git diff --check`. `git archive` aborts the whole export when any listed path does not exist at that commit.

### 4.3 Post

```bash
date -u +%FT%TZ
docs/reviews/tools/review.py post <iid> "$S/note<iid>.md" --head <sha-reviewed> --record
```

`--head` refuses to post if the MR moved since the review (agent pairs often push a fix minutes after opening; re-review the new head instead). `--record` writes the note id **and the reviewed head** into the state file at once, so a crashed session loses nothing and the next `status` shows the MR as UNCHANGED. The GitLab approve button is **not** clicked; the verdict lives in the note text. When a pipeline was still running at posting time and the result matters, post a short **CI addendum** note once it finishes (same prefix, `--any-head`, which leaves the recorded head alone), and record it with `--record` too. If the pipeline is still running at the run's close, the addendum is the first thing the next run posts.

### 4.4 Watcher alerts

When `watch-refs.sh` exits 0 inside the window:

- a new `refs/merge-requests/<iid>/head` or a moved one: run `review.py status`, review it (§4.2), post (§4.3);
- a branch-only change: still re-run `status`; the MR ref can lag the branch push and hide in the new baseline;
- out-of-scope changes (inttest MRs, maintainer wip branches, `testing/*` build branches): note them in the report, no review.

Then re-arm the watcher on a **fresh baseline file** for the remainder of the window (`-m <minutes left>`). The window is 20 minutes from the run start. The operator may extend or shorten it, as a one-off for that run: start a replacement watcher on the **same** baseline file with `-m` set to the minutes to the new end (the script reuses a non-empty baseline, so there is no gap), then stop the old one by PID. A finished run is never reopened. Stop the watcher at the end of the run if it is still polling.

### 4.5 Close

1. Final `review.py status` (heads can move while the report is being written).
2. CI on the target and master: latest pipeline on each, the tenant build branch `testing/t/build/<n>/tenants/<ver>` if one exists, standing advisory reds, open CI work items. Jobs that run only on protected master or tags (the `worker-*-scan` Trivy jobs) are checked on the first master pipeline after a merge that changes them.
3. Write the report (§6) to `docs/reviews/OPEN-MR-ADVERSARIAL-REVIEW-<date>-run<N>.md`.
4. Write the fragment `$S/fragment.json` (§7) and merge it: `review.py merge-state $S/fragment.json` (`--dry-run` first).
5. Delete the heavy scratch; keep the notes and fragment until the merge is confirmed.
6. Tell the operator: what was reviewed, verdicts and note ids, what was skipped, the merge order, and anything unusual (a tool that failed, a window that was unwatched, an addendum deferred).

## 5. Note format

```
## Adversarial review (run <N>, prefix <PP>) — head `<sha9>`, target `<sha9>`

**Verdict: APPROVE | APPROVE WITH NOTES | APPROVE WITH NITS | REQUEST CHANGES (narrow) for PP-1 | REQUEST CHANGES | BLOCK.**
One paragraph: the one-line reason, the CI state, the merge condition.

Scope: commits, files, +/-; what the MR does; lane rule; merge-tree result against the target and every open sibling.

### Verified            (for re-attempts and moved heads: disposition table of every prior finding ID first)
- bullets with file:line anchors

### Findings
**PP-1 (High): title.** evidence with file:line, consequence, concrete fix.
**PP-2 (Info): …**

**Security:** one line on listeners, auth, secrets, sandboxing, approvals, outbound HTTP (or "no change").
**Merge readiness:** condition, in one line.
```

CI addendum: `**CI addendum (run N, prefix PP) for head \`sha9\`.** pipeline, counts, the red jobs and whose they are, whether the verdict changes.`

Severities are strict by request of the operator: a Blocker is something that must not merge; High is a real defect on a reachable path; Medium is a defect with a workaround or a narrow path (an opt-in feature that does not work as documented on one of its intended clients is Medium); Low is a carried risk, an operational gap (a setting the tooling drops), or a cosmetic defect in a security-relevant place; Info is everything the author should know but need not act on. When a later run finds an earlier finding wrong, withdraw it explicitly in the new note, and correct it on the work item too if the finding was filed there.

## 6. Report format

Header line with run number, times, target and master heads. Then: scope paragraph (what changed since the last run, what is out of scope), next prefix, a summary table (MR, item/WI, author and branch, head and whether it changed, verdict with run and note ids, merge status, pipeline), one section per reviewed MR (scope, verified, disposition, findings, verdict, note id, time), one short section per skipped MR (head, standing verdict, pipeline change since), **merge order** (sequential, with conditions), CI on the target and master, the window (start, baseline time and count, alerts, exit code, final snapshot), and whether any harness safety check fired. Quiet runs still get a report (runs 325, 327 and 334 are small examples; runs 324 and 330 are large ones).

## 7. The state file and the run fragment

### 7.1 Contract

`docs/reviews/open-mr-review-state.json` is one JSON object. Keys starting with `_` are ignored everywhere.

| Key | Written by | Meaning |
|---|---|---|
| `last_run` (int) | `merge-state` | the number of the last closed run; the next run is `last_run + 1` |
| `last_run_at`, `report` (str) | `merge-state` | close time (`YYYY-MM-DDTHH:MMZ`) and report path of that run |
| `next_prefix` (two capitals) | `merge-state` | the prefix the next review consumes; `AA`…`ZZ`, continuing across releases |
| `target_heads` (`{"run<N>": sha9}`) | `merge-state` | the target head at each run's close |
| `previous_reports` (list) | `merge-state` | every report path, oldest first |
| `mrs.<iid>.head_sha` (full sha) | `post --head --record`, `note-id --head`, fragment | **the head that was last fully reviewed**; `status` compares it with the MR's head: equal = UNCHANGED (skip), different = CHANGED (re-review), absent = NEW |
| `mrs.<iid>.prefix`, `.run`/`.posted_run`, `.verdict`, `.findings`, `.pipeline`/`.ci` | fragment | the last review's prefix, run, verdict line, one-line finding summary, pipeline summary |
| `mrs.<iid>.posted_note_ids` (list), `.posted_note_id`, `.posted_at` | `post --record`, `note-id`, fragment (unioned) | every note the loop posted on the MR, the first one, the last time |
| `mrs.<iid>.item`, `.title`, `.author`, `.source`/`.source_branch`, `.target` | fragment | identification |
| `mrs.<iid>.rebased`, `.merged`, `.closed` | fragment or hand edit | free text for what the operator did with the MR (who, when, which sha) |
| `runs[]` | `merge-state` (append) | one entry per run: `run`, `start`, `end`, `target`, `master`, `window`, `mrs_reviewed`, `mrs_skipped`, `open_mrs`, `out_of_scope`, `notes_posted`, `prefixes_used`, `verdicts`, `ci`, `merge_order`, `follow_ups`, `auto_mode_block` |

Rules the tools enforce:

- `merge-state` refuses a fragment whose `last_run` is not exactly `state.last_run + 1` (double merge, or a missing run) unless `--force` is given; even with `--force` it refuses a second `runs` entry for the same run number. To add to an already-merged run, edit the state by hand (back it up first).
- `merge-state` merges `mrs.<iid>` **key by key** (a fragment entry with only `pipeline` and `notes` touches nothing else), unions `posted_note_ids`, appends `runs_entry`, sets the scalars that are present, and records `target_heads[run<N>]` from `target_head`.
- `post --head <sha> --record` sets `head_sha` to the MR's head (which `--head` has just verified equals the reviewed one). `post --any-head --record` records the note only. `note-id --head <sha>` sets `head_sha` too.
- `status` is read-only; `prefix` does not consume prefixes.

Hand edits are allowed when the tools cannot express a change; keep the JSON valid (`python3 -c 'import json;json.load(open(...))'`), keep a backup in the scratch directory, and say so in the report.

### 7.2 The fragment

The run's state changes are written to one JSON file and merged with `review.py merge-state`:

```json
{
  "_howto": "merged at run close",
  "last_run": 333, "last_run_at": "2026-11-05T15:57Z",
  "report": "docs/reviews/OPEN-MR-ADVERSARIAL-REVIEW-2026-11-05-run333.md",
  "next_prefix": "VX", "target_head": "89f197538",
  "mrs": {
    "2212": {"item": "42 (WI#546 …)", "author": "sweetiebot", "source_branch": "sweetiebot/kimi-42-20261105-1351",
             "head": "bd7f78407", "head_sha": "<full sha>", "prefix": "VW", "run": 333,
             "verdict": "APPROVE", "notes": [14090, 14092, 14095], "pipeline": "7234 …", "findings": "VW-1 Info …"}
  },
  "runs_entry": {"run": 333, "start": "2026-11-05T15:35:47Z", "end": "2026-11-05T15:57Z",
                 "target": "aragents/kimil00p/v2.1.8.0 89f197538", "master": "41f7a9bd9", "window": "…",
                 "mrs_reviewed": [2212], "mrs_skipped": [], "open_mrs": 1, "out_of_scope": ["…"],
                 "notes_posted": {"2212": [14092, 14095]}, "prefixes_used": {"2212": "VW"},
                 "verdicts": {"2212": "APPROVE"}, "ci": "…", "merge_order": ["…"], "follow_ups": ["…"],
                 "auto_mode_block": "did not fire"}
}
```

Write the fragment with placeholders early in the run and fill them at the close. If a session dies before the merge, the next session merges the fragment first (§4.1).

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
- Commits authored "Paseo Lunarwing" are the operator's own agents; provenance is not a finding, and such a commit may land on another agent's branch.
- The standing "latest master pipeline" goals item changes state at the maintainer's hand; its MRs are the maintainer's.
- The opencode2 candidate lane is hands-off; a CI fix there is proven only by a green protected pipeline on master.
- Kimi's in-image `npm test` gate is known to be load-sensitive (`lunarwing_bridge_test.js`, NQ-3); a flake there is a retry, not an MR finding.

## 9. Hazards that cost runs

- **Harness safety classifiers.** Some harnesses have a content-triggered safety check that, once tripped, blocks tool calls for the rest of the session. In run 309 (three sessions in a row) it blocked the shell, then the GitLab tools (reads too), then file edits, each time after the review had read the opencode worker exposure/tunnel material; the identical post went through the moment the operator switched the harness's automatic mode off. So: do the shell and GitLab work early in a run, keep every draft on disk, write the fragment as you go, and when it fires ask the operator to change the harness setting rather than starting a fresh session (which re-trips as soon as it re-reads the material). Report in the run report whether it fired.
- **Two sessions on one run.** Check `ls -la docs/reviews/` timestamps before writing, use the run number in every filename, merge the state file.
- **Heads move right after an MR opens.** Agent pairs push fixes within minutes; `post --head` exists for this. A head that moves during the window gets the next prefix and a disposition table.
- **Trails overclaim.** "Verified", "all green", "carries upstream commit X", "the TUI replaces the text" are checked, not copied: in run 309 the "carries `61845b0`" claim was a content-equivalent backport, not an ancestor; in run 332 the TUI claim had no `tui/` change behind it.
- **Infrastructure reds look like MR reds.** See §4.2 step 6. Three jobs failing at the same minute on `curl: (22)` is a download outage; a worker build killed at an apt step is the snapshot service. Name the cause, cite the passing sibling pipelines, suggest a retry.
- **Exports are not checkouts.** `git archive` output has no `.git`: tests that call `git` fail there (the six `test_main_*` cases of `test_goal1_ci.py`), and mt-admin harnesses need `ic-infrastructure-health-check` exported beside `ic/scripts`.
- **Cold cargo.** A full test run on a cold target dir takes 30-40 minutes and competes with the operator's builds; CI's logs plus targeted read-only gates (`check_no_panics.py`, boundaries grep, `rustfmt --check` on an exported module) usually suffice. Never two cargo processes at once.
- **Credential files.** The MCP token file is the harness's; give scripts their own `GITLAB_TOKEN_FILE` and never echo it. systemd unit environments are secret-bearing: redact them in anything user-facing.
- **Background processes.** Capture the PID of every background watcher; a harness's own task ids are not visible to the next harness. Do not `pkill` by name on the operator's host.

## 10. Tool reference

```
review.py status [--json]                  open MRs vs state: NEW / CHANGED / UNCHANGED, merge status, head pipeline
review.py jobs <pipeline>                  status counts + non-success jobs (truncated: use the API for the full list)
review.py post <iid> <file> --head <sha> [--record] [--dry-run]   new discussion thread; refuses a moved head;
                                                                  --record stores the note id and the reviewed head
review.py post <iid> <file> --any-head [--record]                 e.g. a CI addendum; --record stores the note id only
review.py note-id <iid> <note_id> [--at T] [--head SHA]           record an id posted by other means
review.py merge-state <fragment> [--dry-run] [--force]
review.py prefix [N]
watch-refs.sh [-m MIN] [-i SEC] [-r REPO] <baseline>              exit 0 change / 124 quiet / 2 error
```

`--state <path>`, `--url`, `--project` and `--target` are global options and go **before** the subcommand (`review.py --state s.json status`). Environment: `GITLAB_TOKEN` or `GITLAB_TOKEN_FILE`; optional `GITLAB_URL` (default `https://git.lunarwing.org`), `GITLAB_PROJECT` (default `lunarwing/lunarwing2`), `REVIEW_TARGET` (default the current target branch), `WATCH_IGNORE_REGEX` (a `grep -E` pattern over `<sha>\t<ref>` lines; the default ignores `testing/*`, the maintainer's `goal<N>cmc`, `cmcgoal<N>` and `vibetui*` branches and the wip MRs !1792/!1799; add `sweetiebot/test-` for the inttest MRs).

## 11. When the operator asks for direct work

The loop is review-only: notes only, the operator merges. The operator sometimes declares a bounded phase of direct work ("direct work mode", "rebase !NNNN for me", "merge these"). Rules that held across those phases:

- Only what was asked, on the branch that was named; the phase ends when the operator says so.
- Rebases happen in a private bare repository with alternates (never in the shared checkout): fetch the target to `refs/base/target` and the branch, `git worktree add`, `rebase`, resolve goals-file conflicts item by item (keep the side that changed the item), verify the non-goals diff with `git patch-id --stable` before and after, run the `ci-config` gates on the result, push with `--force-with-lease=<ref>:<reviewed sha>`, post a short rebase note on the MR, and set `head_sha` in the state so the next run does not re-review an unchanged change.
- Merges go through the API with the `sha` pinned to the reviewed head, in the order the review gave; a merge right after a push can answer 405/409 while GitLab is "checking", so re-read and retry.
- Commits made by the agent carry the operator's chosen author identity and the trailers the harness prescribes; the goals file is edited only on an explicit request, in the operator's own wording ("last attempt was on <MR> but has some issues - please go through the review comments on the closed MR and re-attempt according to the review").
- The review state records what happened (`rebased`, `merged`, `closed` on the MR entry) so the next run's summary can say it.
