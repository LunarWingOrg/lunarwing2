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
