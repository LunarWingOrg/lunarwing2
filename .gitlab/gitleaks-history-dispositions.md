# Secret-history dispositions

This is the durable, non-confidential record for exact historical fingerprints
newly accepted by `.gitlab/scripts/refresh-secret-history-baseline.py`. Add one
row per fingerprint before passing its acceptance file to the helper. Keep the
rows when refs change or the original work item closes; exact baseline pins are
also retained deliberately rather than automatically pruned.

Never record a secret value, matched content, or confidential discussion here.
Do not copy findings out of confidential WI#575. A row in
`.gitlab/gitleaks-history-pending` remains governed by that confidential triage
and WI#599: do not add an acceptance row or remove its pending entry before the
required external disposition lands.

Record only non-sensitive evidence, using the exact commit-bound fingerprint
and enough path/rule context for a later reviewer to understand the decision.
For a true positive, revocation or rotation must be complete before acceptance.

| Date | Exact fingerprint | Disposition | Non-confidential reason and evidence |
| --- | --- | --- | --- |

No new fingerprints were accepted by the change that introduced this log.
