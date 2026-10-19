# Goal #69 / WI#400 — salvage context (2026-10-19)

For any agent (kimi harness or Hermes sibling) re-attempting goal #69.

**Previous attempt MR !1769 got REQUEST CHANGES** — CI FAILS, "nowhere
near an acceptable attempt" per the operator's updated goal text.
**Read the MR comments FIRST** and address exactly what the reviewer
flagged. Do not re-assemble the old shape.

Potentially salvageable prior work (use only if the review's asks align):

- Branch `sweetiebot/kimi-69-20261019-0205` (on origin) carries the
  WI#400 KW-series commits:
  - KW-3: trim the root bridge's capability set on every worker run path
  - KW-4: hand newly created cron project dirs to the task uid
  - KW-5: document rootful multi-tenant pebble as unsupported
  - KW-6: keep the tool-proxy directory root-owned 0711, chown only the socket
  - KW-7b: pin the kimi websocket-mode setpriv drop; entrypoint pin carries --no-new-privs
- Uncommitted residue sits in `git stash@{0}` on the Sigma tree
  (~/lunarwing2, labeled "item-69 WI#400 KW-series residue"):
  test-worker-llm-entrypoints.sh, pebble bridge.rs + main.rs mods.
  Inspect with `git stash show -p stash@{0}`; do NOT blind-restore.

Mirrors: Hermes skill `kimi-goal-69-salvage` + mem0 (tag goal-69).

**OUTCOME (2026-10-19, SweetieBot, branch `sweetiebot/kimi-69-20261019-1505`):**
goal 69 completed. The salvage branch was merged (one executor.rs conflict,
hand-resolved) and every run-166 LD finding (LD-1..LD-17) addressed
in-tree; see the item-69 DONE trail in AGENT_GOALS_2.1.6.0.md for the
per-finding record and the verification matrix. The stash was inspected
and salvaged selectively (the token-layout harness test and the LD-10
shape informed the final resolver, which landed as the reviewer's
testable-arguments prototype instead of the stashed self-exec form).
