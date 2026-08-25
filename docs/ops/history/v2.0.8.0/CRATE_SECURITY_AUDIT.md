# Crate Security Audit - v2.0.8.0

**Date:** 2026-08-14

**Source commit:** `24e8b319f7f2` (branch `aj/kimi-6-20260814-0601`)

**Scope:** all 19 tracked Cargo lockfiles

**References:** `CRATE_SECURITY_AUDIT_v2.0.7.0.md`, GitLab work item 53
("rustsec adv — update as many as possible")

## Conclusion

A fresh RustSec scan against all 19 tracked lockfiles found **no new
advisories**. The shipped LunarWing graph still carries exactly the seven
accepted advisories already listed in `ic/deny.toml`; the standalone XMPP
bridge reproduces five of them through its LunarWing path dependency, and the
vendored Signal wrapper carries the two `failure` findings. All other 16
lockfile roots are clean.

A fresh upstream re-check shows that **no currently available crate upgrade
clears any of the seven findings**:

- `rsa` has no patched release — `0.10.0-rc.18` is still the newest published
  version, and `russh 0.62.6` (the newest russh) pins it exactly;
- `libsql 0.9.30` (latest stable) and `0.10.0-pre.4` (latest prerelease) both
  still depend on `hyper-rustls ^0.25`, which keeps the vulnerable
  `rustls 0.22` / `rustls-webpki 0.102` chain alive — even a full libSQL
  migration would not clear the four webpki advisories today;
- `failure 0.1.8` remains the final, unmaintained release used by the vendored
  `libsignal-protocol` wrapper.

The modern TLS side of the graph is fully patched: `rustls-webpki 0.103.14`
(latest), `rustls 0.23.43`, single `reqwest 0.13.4`. `wasmtime 36.0.13`
attracts no advisory.

`cargo update --dry-run` found 19 compatible maintenance updates; none touches
an advisory path. Applying them is deferred to checklist item 10 (the routine
release lockfile refresh), which owns `cargo update` for this release.

**Recommendation: no `deny.toml` change and no advisory-driven upgrade is
possible for v2.0.8.0.** Keep the RSA, libSQL TLS, and vendored Signal
migrations as separate, security-reviewed work.

## Method

- CI-pinned `cargo-deny 0.20.2` with a freshly fetched RustSec advisory
  database (2026-08-14).
- CI-equivalent main check passed:

  ```text
  cd ic
  taskset -c 0-11 cargo deny check advisories
  advisories ok
  ```

- Every tracked lockfile root was then scanned with an **empty advisory
  ignore list** (`--config` override) so the accepted findings are visible
  rather than suppressed. Results:

  | Root | Result |
  |---|---|
  | `ic` | FAILED — the 7 accepted advisories (see table below) |
  | `ic/bridges/xmpp-bridge` | FAILED — 5 of the 7 via LunarWing path dep (rsa + 4 webpki) |
  | `ic/vendor/libsignal-protocol` | FAILED — 2 `failure` advisories |
  | other 16 roots (darkirc, socket clients, gotify copies, multica, xmpp channel, openclaw bridge, test-echo-tool, weechat relay, pebble, pebble4lunarwing, ocr/voice sidecars, replv2git client) | `advisories ok` |

- `cargo update --dry-run --verbose` in `ic/` reviewed for compatible updates.
- crates.io API re-check of `rsa`, `russh`, `libsql` (0.9.30 and
  0.10.0-pre.4 dependency metadata), `rustls-webpki`, `hyper-rustls`,
  `rustls`, `failure`, `wasmtime`.

## Active Shipped-Graph Findings (unchanged from v2.0.7.0)

| Advisory | Locked path | Security assessment | Upgrade status (re-verified 2026-08-14) |
|---|---|---|---|
| `RUSTSEC-2026-0049` | `libsql 0.6.0 -> hyper-rustls 0.25.0 -> rustls 0.22.4 -> rustls-webpki 0.102.8` | Incorrect CRL distribution-point handling. Relevant only to the optional libSQL remote/TLS path. | Still blocked: `libsql 0.9.30` and `0.10.0-pre.4` both keep `hyper-rustls ^0.25`. |
| `RUSTSEC-2026-0098` | same libSQL chain | URI name constraints accepted incorrectly after certificate signature verification. | Same block. |
| `RUSTSEC-2026-0099` | same libSQL chain | Wildcard certificates can bypass a permitted-subtree name constraint. | Same block. |
| `RUSTSEC-2026-0104` | same libSQL chain | Reachable panic parsing a CRL; applications not using CRLs are unaffected. | Same block. |
| `RUSTSEC-2023-0071` | `russh 0.62.6 -> rsa 0.10.0-rc.18` | Marvin RSA timing side channel. The shared `russh` SSH-agent path can sign with configured RSA keys for workers/`ssh_git`. | Still unpatched upstream: `rsa 0.10.0-rc.18` is the newest release; `russh 0.62.6` (locked) is the newest russh. |
| `RUSTSEC-2019-0036` | vendored `libsignal-protocol 0.1.0 -> failure 0.1.8` | `failure` type confusion; the wrapper derives `Fail` and does not override the private type-ID method, so it is not affected per RustSec, but the obsolete dependency ships in OMEMO code. | No newer `failure` release exists (0.1.8 is final). Requires replacing the vendored wrapper's error API. |
| `RUSTSEC-2020-0036` | same vendored Signal chain | `failure` is end-of-life/unmaintained. | Same as above. |

`cargo tree --invert rustls-webpki@0.102.8` confirms the vulnerable copy is
reached only through `libsql 0.6.0` (`hyper-rustls 0.25.0`, also via
`tokio-rustls 0.25.0`). The main graph's modern consumers use the patched
`rustls-webpki 0.103.14`.

## Upstream Re-Check Details

- **`rsa`**: newest published version is still `0.10.0-rc.18`; RustSec still
  lists no patched release for the Marvin advisory. Nothing to upgrade to.
- **`russh`**: locked `0.62.6` equals the newest crates.io release, so there is
  no russh upgrade pending; it continues to pin the affected `rsa` release.
  The only clearing path remains a policy change (reject RSA keys at ingestion
  and in the shared agent, then drop the `russh/rsa` feature) — a dedicated
  migration, not a lockfile edit. Operators should use Ed25519/ECDSA keys.
- **`libsql`**: `0.9.30` dependency metadata requests `hyper-rustls ^0.25`
  (optional); `0.10.0-pre.4` metadata is identical. Upgrading libSQL therefore
  does **not** remove `rustls 0.22` / `rustls-webpki 0.102`. Recheck when
  libSQL moves to an `hyper-rustls` line built on `rustls 0.23` /
  `rustls-webpki >= 0.103.13`. PostgreSQL remains the preferred backend;
  libSQL users should avoid remote/TLS mode where possible.
- **`rustls-webpki` / `rustls`**: locked modern copies (`0.103.14`, `0.23.43`)
  are already the newest releases.
- **`failure`**: `0.1.8` remains the final release; the vendored Signal wrapper
  migration stays on the security-reviewed backlog.
- **`wasmtime`**: locked `36.0.13` has no open advisory (it already fixes
  `RUSTSEC-2026-0222`). `47.0.3` is available but crosses eleven major
  component-model/sandbox API releases — scheduled maintenance, not a
  security action.

## Compatible Update Review (`cargo update --dry-run`)

19 manifest-compatible updates are pending in the main lockfile:
`generic-array 1.4.4 -> 1.4.5`, `http-body-util 0.1.4 -> 0.1.5`,
the `icu_* 2.2.0 -> 2.3.0` family (8 packages), `litemap 0.8.2 -> 0.8.3`,
`num-integer 0.1.46 -> 0.1.47`, `potential_utf 0.1.5 -> 0.1.6`,
`rangemap 1.7.1 -> 1.8.0`, `tinystr 0.8.3 -> 0.8.4`, `whoami 2.1.2 -> 2.1.3`,
`writeable 0.6.3 -> 0.6.4`, `zerotrie 0.2.4 -> 0.2.5`, and the
`zerovec 0.11.6 -> 0.11.7` family.

**None of these touches an advisory path**, so applying them is routine
maintenance. It is deferred to checklist item 10, which performs the release
lockfile refresh (`cargo update`) for v2.0.8.0 anyway; doing it twice would
only create duplicate churn.

Notable "unchanged, newer available" entries re-confirmed for the backlog:
`rig-core 0.40.0 -> 0.41.0`, `tokio-tungstenite 0.26.2 -> 0.30.0`,
`wasmtime`/`wasmparser` (see above), the RustCrypto set (`aes-gcm 0.11`,
`hkdf`/`hmac 0.13`, `sha2 0.11`, `ed25519-dalek 3`, `x25519-dalek 3`),
`rand 0.10.2`, `html-to-markdown-rs 3.11.0`, `zbus 5.19.0`, `zip 8.6.0`.
Dispositions from the v2.0.7.0 backlog table still apply.

## Vendored and Feature-Only Note

The vendor scan above ran with the wrapper's default (`crypto-native`)
feature. As documented in the v2.0.7.0 audit, enabling its `crypto-openssl`
feature additionally reports `RUSTSEC-2021-0134` (`rental` unmaintained); that
crate is not in the shipped graph and still does not belong in `ic/deny.toml`.

## Release Recommendation

1. Do not add or remove an advisory exception for v2.0.8.0 — the accepted set
   of seven is unchanged and still accurately described by `ic/deny.toml`
   comments.
2. Do not claim `cargo-deny` is clean without noting the seven findings are
   accepted by policy.
3. Do not attempt an RSA, libSQL TLS, or vendored Signal migration inside the
   release checklist; none is currently unblockable by a crate upgrade anyway.
4. Let checklist item 10 apply the 19 compatible maintenance updates during
   its lockfile refresh, then re-run the CI advisory check before tagging —
   RustSec data changes independently of source.

## Follow-up (item 10, 2026-08-14)

Checklist item 10 ran the release lockfile refresh in `ic/`: the deferred
maintenance updates landed (the full `icu_*`/`zerovec` families,
`generic-array`, `http-body-util`, `litemap`, `num-integer`, `potential_utf`,
`rangemap`, `tinystr`, `whoami`, `writeable`, `zerotrie`, plus patch releases
published since the audit: `cc 1.4.3`, `find-msvc-tools 0.1.11`,
`libredox 0.1.20`, `pkg-config 0.3.34`) alongside the 2.0.8 manifest bumps.
Re-ran the CI advisory check (cargo-deny 0.20.2, fresh advisory fetch):
`advisories ok, bans ok, licenses ok, sources ok` — the seven accepted
findings remain and no new advisory appeared.

## Follow-up (item 19, 2026-08-18)

Work item 53 re-verification on branch `rarity/kimi-19-20260818-1200`. A fresh
RustSec fetch shows the landscape changed since 2026-08-14: one **new**
advisory now hits the shipped graph.

### New advisory: RUSTSEC-2026-0258 (h2 unbounded empty DATA frames, low)

Empty DATA frames are accepted and queued without limit; undrained streams can
grow memory unboundedly or panic on length overflow. **Patched only in
`h2 0.4.16`** — the 0.3 line ended at `0.3.27` with no backport (re-verified
against crates.io 2026-08-18).

Three lockfile roots carried `h2`:

| Root | Before | Action | After |
|---|---|---|---|
| `ic` | `0.4.15` + `0.3.27` | `cargo update -p h2@0.4.15` → `0.4.16`; `0.3.27` only reachable via the legacy `libsql 0.6.0 -> tonic 0.11 -> hyper 0.14` chain, so it joined the accepted set in `ic/deny.toml` with an explanatory reason | patched 0.4 line + accepted 0.3 remainder |
| `ic/bridges/xmpp-bridge` | `0.4.15` + `0.3.27` | same 0.4-line bump; the 0.3 copy is the same inherited libsql chain and is covered by the `ic/deny.toml` exception cargo-deny picks up from the workspace root | `advisories` check exit 0 |
| `projects/ocr-sidecar` | `0.4.15` + `0.3.27` | `0.3.27` came in via **`warp 0.3.7`** (not libsql); upgraded `warp 0.3 -> 0.4.3` (hyper 1.x line, new non-default `server` feature enabled), which **eliminated the entire hyper 0.14 / h2 0.3 tree** from that lockfile | no h2 0.3 left; `advisories ok` with zero exceptions |

### Verification (2026-08-18, cargo-deny 0.20.2, fresh advisory DB)

- `cd ic && cargo deny check` → `advisories ok, bans ok, licenses ok, sources ok`.
- Empty-ignore rescan of `ic/`: exactly 8 vulnerability/unsound findings — the
  original 7 accepted plus the h2 0.3.27 remainder; the `0.4.16` copy is no
  longer flagged. No further new advisories.
- `xmpp-bridge`: `cargo metadata --locked` ok; deny advisories exit 0.
- `ocr-sidecar`: `cargo check` green on warp 0.4.3, `cargo test` 4/4 pass,
  `advisories ok`.
- `ic/` workspace `cargo check --locked` (taskset-bounded) green on the
  bumped lockfile.
- All 19 tracked lockfiles rescanned for `h2`: only the three roots above ever
  carried it; all now lock `0.4.16` (ocr-sidecar additionally dropped 0.3.27).
- Upstream re-check: no `h2 0.3.x` patch exists (0.3.27 is the newest 0.3
  release; the advisory lists 0.4.16 as the only fix); clearing the `ic`
  0.3.27 copy remains blocked on the same libsql migration as the webpki
  findings.

### Accepted-advisory count

`ic/deny.toml` now accepts **8** findings: the original seven plus
`RUSTSEC-2026-0258` scoped to the libsql legacy chain. The ocr-sidecar warp
upgrade means one of the three roots needs no exception at all.
