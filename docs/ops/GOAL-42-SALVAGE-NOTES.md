# GOAL-42 SALVAGE NOTES (written 2026-10-26 ~02:05 UTC by SweetieBot interactive lane)

## What happened
Run `sweetiebot/kimi-42-20261025-1205` (item #42 / WI#383) ended SUCCESS (MR !1919),
but left UNCOMMITTED changes to `ic/src/orchestrator/harness.rs` in the shared worktree.
Every subsequent launch tick (15:05, 18:05, 23:34, 00:05) died at the checkout gate:
"Your local changes ... would be overwritten by checkout: ic/src/orchestrator/harness.rs".

## Salvage location
The residue is in labeled STASH (carrier rule: stash, NOT worktree):
  stash@{0}: "item-42 residue pre-9b506e942 resync (harness.rs, uncommitted mods found 2026-10-26 ~02:00 UTC blocking checkout; preceding run 12:05 SUCCESS MR !1919)"
Worktree was reset --hard to origin/aragents/kimil00p/v2.1.7.0 @ 9b506e942 (origin parity verified).

## Governing rule for the next item-42 run (repass)
- READ this file first (loop prompt carries the ALSO-CHECK line).
- `git stash list` to confirm stash@{0} is still the item-42 entry (positions shift!).
- Pop the stash ONLY into a clean worktree after the loop's reset --hard, i.e. let the
  run itself re-apply it on its item branch — never pre-apply to the board branch
  (the reset would eat it).
- What the diff does: see the stash itself; WI#383 review context is on the tracker
  (MR !1919 comments). Verify the review findings before trusting the residue's
  direction — this stash predates the !1919 review feedback.
- If the residue conflicts with the new board state, prefer the board; re-stash any
  unusable remainder with a labeled message.
