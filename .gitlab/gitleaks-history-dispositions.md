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
| 2026-11-12 | `54470b308fe4b857402c58bd8105b299847a9af7:ic/vendor/russh/.cargo-checksum.json:generic-api-key:1` | False positive | Vendored `russh` 0.65.1 (WI#644). The file is cargo's checksum manifest: SHA-256 digests of the crate's files. The package digest in it equals the `russh` checksum in `ic/Cargo.lock`. Same class as the existing `.cargo-checksum.json` pins of other vendored crates. |
| 2026-11-12 | `54470b308fe4b857402c58bd8105b299847a9af7:ic/vendor/russh/src/helpers.rs:private-key:286` | Upstream test fixture | Vendored `russh` 0.65.1 (WI#644). A constant `RSA_OPENSSH` inside the crate's `#[cfg(test)] mod tests`: a public test key that upstream ships to prove that an RSA key is refused when the `rsa` feature is off. It guards no system, and LunarWing builds without that test module. |

No new fingerprints were accepted by the change that introduced this log.
