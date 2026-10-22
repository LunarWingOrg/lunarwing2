# GOAL-93 SALVAGE NOTES — WI#456 / WI#461 chown-guard work

## RESOLVED 2026-10-22 (branch `sweetiebot/kimi-93-20261022-1828`)

The salvaged work was applied, corrected, and landed as commit
`117a79db5` ("item-93: WI#461 guard prepare_worker_workspace chown
walk"), followed by WI#462/WI#463 (`df89b66d6`), the WI#456
investigation + temp-tenant verification (docs/ops/
ITEM93_KIMI_WORKER_456.md + WORKER-CONTAINERS runbook), and the goals
check-off (`8f8d3eebc`). Key corrections made to the salvage:

1. The harness `chown()` FUNCTION MOCK shadows the PATH stub and
   swallows failures — the salvaged pin could never fire. The landed
   pin unsets the mock for its block and restores it after. The
   "stub log stayed empty / pin didn't fire" mid-debug mystery from
   the timeout run was exactly this.
2. The pre-existing MI-3 harness failure on the noexec-/tmp dev VM was
   RETRACTED as a WI#461 causal claim by run-195 review (NB-8): it is a
   TEST-ENVIRONMENT artifact (the stub cannot execute from a noexec
   tempdir, so `find -exec` falls through to the real chown). The harness
   stub-derived pins now self-skip with a named reason on noexec
   tempdirs. The WI#461 guard stands on its own merits (clean rc-1
   refusals + PWD restore).

The stash `stash@{0}` ("item-93 WI#461 chown-guard work ...") is
superseded by the landed commits and can be dropped by an operator.
WI#456's root cause (stale pre-WI#400 tenant-store kimi image +
admin store empty -> image-user guard die -> daemon route hot against
a dead socket) and its remediation are recorded in
docs/ops/ITEM93_KIMI_WORKER_456.md.

## What happened (original run, kept for the record)
Run kimi-93-20261022-1805 (18:05 tick, item #93 = WI#456) hit the harness
timeout after ~12 min of real work (kimi log: turn.step 0.30, last assistant
message mid-debug of the MI-3 log stub) and exited with zero commits. The
loop recorded "No commits — kimi finished without work", counter item-93 = 1.
The uncommitted changes were salvaged to git stash BEFORE any reset:

    stash@{0}: "item-93 WI#461 chown-guard work (timeout run kimi-93-20261022-1805, uncommitted salvage)"

## What the salvaged work contained (2 files, +43/-3)
1. ic/scripts/mt-admin-lib/09-worker-config.sh — prepare_worker_workspace():
   both chown steps (root dir + find-walk) now guarded with `if ! chown ...`
   clean rc-1 refusals instead of bare set -euo pipefail aborts from inside
   the function, PLUS caller-PWD restore on every refusal exit path (MV-4).
   WI#461 labeled in comments.
2. ic/scripts/tests/test-mt-admin-kimi.sh — new WI#461 pin block: PATH-stub
   chown that exits 1, asserts prepare_worker_workspace returns clean rc 1
   (not a set -e abort) and restores the caller PWD (MV-4 pin).

## What was in flight when it died
The harness was mid-debug on the MI-3 pin: the stubbed-chown log stayed
empty and the WI#461 pin didn't fire — it was testing `find -exec chown`
behavior as root to figure out why the stub wasn't being hit. The stub
approach itself may need rework: find -exec with a stubbed chown may not
invoke the stub the way the test assumed (or the -links +1 filter skipped
the fixture files). Next run: read this note first, review the pin's
mechanism before trusting it, then commit what stands.
(Resolution: the function mock was the culprit — see RESOLVED above.)

## Governing rule
Read this note before re-attempting; salvage only what aligns with the
review verdicts; leave the stash apply/re-commit decision to the next run.
(The next run has come and gone; this note is now historical.)
