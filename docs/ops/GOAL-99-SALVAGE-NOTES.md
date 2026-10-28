# GOAL-99 Salvage Notes

## What this is
Item #99's prior attempt (branch `sweetiebot/kimi-99-20261028-0605`, run 06:05
UTC Oct 28) produced a complete SUCCESS-quality payload but its MR was closed
for merge conflicts. Operator ruling (board item text, Oct 28): do NOT repeat
the test — build a NEW MR without conflicts, referencing the original task.
The tenant **mistyfly** must keep running (do not destroy; non-animal name).

## The salvageable branch (do not rebuild the work)
`sweetiebot/kimi-99-20261028-0605` — 8 commits ahead of old base `62bed7142`:
- `docs/ops/ITEM99_TEST_TENANT_MISTYFLY.md` — full findings report (F1 source-root
  asymmetry, F2 WI#424 egress fail-closed deadlock, F2b orphaned lifecycle lock,
  F5 NanoGPT allowlist gap)
- `ic/scripts/mt-admin-lib/15-worker-egress.sh` + regression test — the
  `worker_egress_launch_args` trailing-`return 0` fix (commit `8cce307a2`),
  verified: 11 failures without, 10 passes with
- a goals-doc edit (DONE trail for #99) — this is the ONLY conflicting file

## Conflict analysis (verified 2026-10-28 ~14:00 UTC)
Dry-run merge against `origin/aragents/kimil00p/v2.1.7.0` (9751abdd0):
- ONLY `docs/ops/AGENT_GOALS_2.1.7.0.md` conflicts (single hunk, lines ~1278).
- All other branch payload (report + egress fix + test) auto-merges clean.
- Cause of the hunk: item #98 was ticked DONE by Rarity in the meantime
  (branch `aj/kimi-98-20261028-0600`, root `RELEASE-v2.1.7.0.md`), and the
  operator re-ticked #99 back to `[ ]` with the re-MR brief.

## Resolution recipe for the redo
1. Branch from CURRENT `origin/aragents/kimil00p/v2.1.7.0` (9751abdd0), not
   the old base.
2. Cherry-pick the branch's payload commits: the mistyfly report, the
   worker-egress fix `8cce307a2`, and its test. Skip the branch's goals-doc
   DONE-trail edit (it conflicts; the DONE trail can be re-added on the new
   branch by editing the CURRENT goals doc — append under #99 after the
   operator's re-tick, do not touch #98/#100/#101 lines).
3. Open the MR against `aragents/kimil00p/v2.1.7.0`, title referencing the
   original task. Set `remove_source_branch: true`.
4. Do NOT repeat the tenant test — mistyfly is running; reference the report.
5. Leave #99 unchecked on the board branch until MR review settles it; the
   loop tick itself does the checkoff on SUCCESS per the normal path.

## OUTCOME — redo completed 2026-10-28 (branch `sweetiebot/kimi-99-20261028-1523`)
Recipe followed; the result is conflict-free (`git merge-tree --write-tree`
against target `66e9433b1` reports no conflicts and lands all payload files).

- Target had moved again to **`66e9433b1`** (past `9751abdd0`); the seven
  payload commits cherry-picked onto it with **zero conflicts**, re-authored as
  SweetieBot. The eighth (`76d2e7779`, goals-doc only) was dropped and its trail
  re-authored against the live document as a single hunk on the #99 line;
  #98/#100/#101 are untouched.
- **The closed MR !2013 carried an adversarial review (run 250, "APPROVE WITH
  NITS") whose asks are now landed.** This matters for any future redo: the
  review is the authoritative list of what this payload still needed, and it was
  *not* in the notes above.
  - QQ-1 (Low) — the report never described the bug its own commit fixes. Added
    finding **F2a**; F5's stale "F2's bug" cross-reference retargeted.
  - QQ-2 (Info, security) — do not silently widen the fail-closed allowlist for
    `--with-pebble`; F5's remedy now recommends the loud refuse-and-name-
    `configure-worker-egress --allow` shape.
  - QQ-3 (Info) — F2b's fix is one step, not two (a flock dies with its open
    file description, so no `trap` is needed); both equivalent variants plus the
    `inherited_lock_fd_is_held` caveat recorded.
  - QQ-4 (Info) — the review filed the four findings; **WI#504** (F2),
    **WI#505** (F2b), **WI#506** (F5), **WI#507** (F1), all confirmed open.
    Cited in the headings plus a finding→owner index table.
  - Plus the review's "worth one sentence" note: F2b is OpenRC-specific.
- Report grew 713 → **823 lines** from that review work. A provenance header was
  added stating the report's SHAs and build logs deliberately still refer to the
  original `0605` run, because they are the measured evidence for F1 —
  rewriting them would invalidate the traceability gap F1 describes.
- All code anchors re-verified against the new target (they had not drifted):
  `05-tenant-build.sh:562/:1074`, `15-worker-egress.sh:241/:396-408/:560-572`,
  `00-common.sh:84/:134-147`, `04-tenant-lifecycle.sh:1018`,
  `lunarwing-mt-admin.sh:16`, `server.rs:1565`, `09-worker-config.sh:1210`,
  `router.rs:3682`, `knowledge_sync.rs:259`. The 13 cited fix commits are all
  ancestors of this branch.
- Verification: `check-mt-admin.sh` green (17 modules); `shell-syntax.sh` 266
  scripts + 3 hooks; `shellcheck.sh` clean at CI severity; `test-worker-egress.sh`
  **109 PASS** with the FAIL set byte-identical to the untouched target's (proved
  via a temporary worktree at `66e9433b1`). No Rust source changed.
- **Do not `git add -A` on this repo root**: `.cargo/advisory-dbs/` (~7 MB) and
  `.cargo/git/` (~271 MB) are untracked transient cargo caches that are *not*
  gitignored and would be swept into the commit. Stage explicitly.
- A stale `.git/sequencer/` (an item-108 `no-commit` pick left by another
  session) blocked `git cherry-pick`; cleared with `git cherry-pick --quit`.

