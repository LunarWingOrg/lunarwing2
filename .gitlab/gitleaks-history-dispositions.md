# Secret-history dispositions

This is the durable, non-confidential record for exact historical fingerprints
newly accepted by `.gitlab/scripts/refresh-secret-history-baseline.py`. Add one
row per fingerprint before passing its acceptance file to the helper. Keep the
rows when refs change or the original work item closes; exact baseline pins are
also retained deliberately rather than automatically pruned. A finding in
`.gitlab/gitleaks-history-pending` must neither receive an acceptance row here
nor be removed from that file before its external disposition lands.

Never record a secret value, matched content, or confidential discussion here.
Do not copy findings out of confidential WI#575. The last row that
`.gitlab/gitleaks-history-pending` held, the `gitlab-pat` example at
`59b4247c2`, was settled by WI#599 on 2026-11-12 and is recorded below; the
pending file now names no rows.

Record only non-sensitive evidence, using the exact commit-bound fingerprint
and enough path/rule context for a later reviewer to understand the decision.
For a true positive, revocation or rotation must be complete before acceptance.

| Date | Exact fingerprint | Disposition | Non-confidential reason and evidence |
| --- | --- | --- | --- |
| 2026-11-12 | `59b4247c2a7984ccd52126e894df98251c10f030:docs/ops/AGENT_GOALS_2.1.6.0.md:gitlab-pat:1270` | Not a token on this instance | A token-shaped example inside a review sentence (WI#599), no longer in the current tree. An instance administrator's `POST /api/v4/admin/token` lookup on 2026-11-12 returned HTTP 404, confirming that it is not a token on this instance (WI#599 note 15857). |
| 2026-11-12 | `54470b308fe4b857402c58bd8105b299847a9af7:ic/vendor/russh/.cargo-checksum.json:generic-api-key:1` | False positive | Vendored `russh` 0.65.1 (WI#644). The file is cargo's checksum manifest: SHA-256 digests of the crate's files. The package digest in it equals the `russh` checksum in `ic/Cargo.lock`. Same class as the existing `.cargo-checksum.json` pins of other vendored crates. |
| 2026-11-12 | `54470b308fe4b857402c58bd8105b299847a9af7:ic/vendor/russh/src/helpers.rs:private-key:286` | Upstream test fixture | Vendored `russh` 0.65.1 (WI#644). A constant `RSA_OPENSSH` inside the crate's `#[cfg(test)] mod tests`: a public test key that upstream ships to prove that an RSA key is refused when the `rsa` feature is off. It guards no system, and LunarWing builds without that test module. |
| 2026-11-12 | `4247229478e0092f219654ab1dff0e977379a815:ic/scripts/lunarwing-mt-admin.sh:generic-api-key:846` | False positive | The `local` declaration that opens the `add-tenants` verb of mt-admin. It makes 33 assignments: 18 empty strings, 11 `true` or `false` values and four references to `DEFAULT_*` variables. Every variable with `api_key` in its name is set to the empty string, and the line holds no other literal. Accepted by the maintainer on 2026-11-12. |
| 2026-11-12 | `f690d36382f77e7133f8e72bfcb4931517b2a44b:lunardepth4lunarwing/scripts/lunarwing_bridge_test.ts:generic-api-key:24` | False positive | A `Sec-WebSocket-Key` header in the hand-written WebSocket handshake of a LunarDepth bridge test. The value is the public example nonce printed in RFC 6455, section 1.3. It is a handshake nonce and not a credential: it authenticates nothing, and the test sends it to a loopback server it starts itself. Accepted by the maintainer on 2026-11-12. |
| 2026-11-12 | `68d8b44ec03ec0e6a2095ce47840ba389eb4242f:lunardepth4lunarwing/scripts/lunarwing_bridge_test.ts:generic-api-key:24` | False positive | The same line as the `f690d3638` row above, re-reported at the commit that last touched the file. Line 24 is byte-identical at both commits (MD5 of the line, `67dbdfad9c0fe2a665875da92be63dd0`, is the same), so the evidence is unchanged: an RFC 6455 section 1.3 example `Sec-WebSocket-Key` nonce that authenticates nothing. Found by a local full-history scan on 2026-11-12 in branch `sweetiebot/kimi-24-20261111-0905`, which is not reachable from any remote ref; pinning it keeps the baseline a superset if that branch is ever pushed. |

No new fingerprints were accepted by the change that introduced this log.

## Historical baseline triage closure (WI#575)

The committed baseline was reviewed as a whole before the per-row acceptance
rule above existed. This section is its durable, non-confidential disposition
record; it names no finding value and no matched content, and every path and
rule it counts is already visible in `.gitlab/gitleaks-history-ignore`.

Snapshot of that file on 2026-11-18: **1,918 exact pins across 52 commits** —
**1,854 under `ic/vendor/`** in 194 crate directories and **64 outside it**.
Rule mix: `generic-api-key` 1,295, `private-key` 589, `jwt` 16,
`square-access-token` 5, `gcp-api-key` 4, `curl-auth-user` 3,
`discord-api-token` 3, `stripe-access-token` 2, `gitlab-pat` 1.

- **Vendored pins (1,854): false positive.** Third-party crate sources, their
  cargo checksum manifests, and the upstream test keys and example tokens that
  ship with them. No LunarWing system trusts them; the history scan still
  covers `ic/vendor/`, and every new pin, vendor or not, still needs its own row
  above.
- **Other pins (64): false positives.** Examples include test and fuzz fixtures,
  public keys and a public WebSocket nonce used in tests, saved public web
  pages, documentation and template examples, a hook comment, a build digest,
  and the empty-key declaration in `lunarwing-mt-admin.sh` recorded above.
- **The `gitlab-pat` row:** the administrator lookup recorded above found no
  token on this instance for the example at `59b4247c2` (HTTP 404). The
  administrator sent the value only to this instance's token-information
  endpoint for the lookup; it was never used to authenticate.
- **No true positive was found**, so no revocation or rotation was required.

### Scheduled-run evidence

On 2026-11-12, [`secret-history` job 174508](https://git.lunarwing.org/lunarwing/lunarwing2/-/jobs/174508)
passed in scheduled pipeline 7727 on branch `sweetiebot/kimi-18-20261112-1005`
at commit `085886814`. The job ran with `allow_failure: false` and reported no
leaks after scanning 11,524 commits (about 2.07 GB). The overall pipeline was
canceled; the scan job itself succeeded.

The temporary schedule was later removed. This run is evidence for that
feature branch, not a fresh scheduled run on the merge target. The protected
release-tag run still needs to be verified, and creating and verifying the
recommended weekly schedule remains an operator action. The merge-request
`secret-history-regression` job exercises the harness on synthetic histories in
the CI-pinned Gitleaks v8.30.1 image; it does not replace either full scan.
