# Crate Security Audit - v2.0.7.0

**Date:** 2026-08-10  
**Source commit:** `c157a53374fa`  
**Scope:** all 19 tracked Cargo lockfiles plus 12 excluded/lockless Cargo roots  
**Reference:** `CRATE_AUDIT_v2.0.5.0.md`

## Conclusion

No crate has a newly available security upgrade that should be applied as a
simple v2.0.7.0 release bump. A live RustSec scan found the same seven accepted
advisories in the shipped LunarWing graph that are already listed in
`ic/deny.toml`; no unlisted advisory was found. The other active lockfile roots
are clean.

The seven shipped-graph findings are not solved by a compatible lockfile update:

- four `rustls-webpki` findings are pinned behind `libsql`'s legacy TLS stack;
- the `rsa` Marvin timing advisory has no patched RustCrypto release and is
  enabled through `russh`;
- two `failure` findings come from the vendored Signal wrapper and have no safe
  upstream upgrade.

These should remain tracked, but none justifies an untested dependency or
backend migration immediately before v2.0.7.0. No manifest, lockfile, or
`deny.toml` change is recommended for item 1.

## Method

The CI-pinned `cargo-deny 0.20.2` installer and a freshly fetched RustSec
advisory database were used. The main CI-equivalent check passed:

```text
cd ic
taskset -c 0-17 ../.ci-bin/cargo-deny check advisories
advisories ok
```

Because that result honors `ic/deny.toml`, every tracked lockfile was also
scanned with `--all-features` against an empty advisory ignore list. Excluded
Cargo roots without committed lockfiles were resolved and scanned separately;
temporary lockfiles were removed afterward. This covered 31 Cargo roots in
all. The stale standalone XMPP bridge lock could not be read with `--locked`
after its path dependency changed, so its currently resolved graph was scanned
without `--locked` and its lockfile was restored byte-for-byte.

A `cargo update --dry-run --verbose` of the main graph was also reviewed. It
found compatible maintenance updates, but none affects the advisory set. Those
ordinary lockfile updates belong to the release lockfile-reconciliation item,
not this security investigation.

## Active Shipped-Graph Findings

| Advisory | Locked path | Security assessment | Upgrade status |
|---|---|---|---|
| `RUSTSEC-2026-0049` | `libsql 0.6.0 -> hyper-rustls 0.25.0 -> rustls 0.22.4 -> rustls-webpki 0.102.8` | Incorrect CRL distribution-point handling. Relevant only to the optional libSQL remote/TLS path. | Patched in `rustls-webpki >=0.103.10`, but `rustls 0.22` requires the 0.102 line. |
| `RUSTSEC-2026-0098` | same libSQL chain | URI name constraints can be accepted incorrectly after certificate signature verification. | Patched in the 0.103 line; blocked by the same parent constraints. |
| `RUSTSEC-2026-0099` | same libSQL chain | Wildcard certificates can bypass a permitted-subtree name constraint after signature verification. | Patched in the 0.103 line; blocked by the same parent constraints. |
| `RUSTSEC-2026-0104` | same libSQL chain | Reachable panic while parsing a CRL, but applications that do not use CRLs are unaffected. | Patched in `rustls-webpki >=0.103.13`; blocked by the same parent constraints. |
| `RUSTSEC-2023-0071` | `russh 0.62.5 -> rsa 0.10.0-rc.18` (also through `ssh-key`) | Network-observable RSA timing side channel. The built-in in-process SSH executor rejects RSA private keys, but the shared `russh` SSH-agent path can still parse and sign with configured RSA keys for workers/`ssh_git`; therefore the exception is not purely dead code. | RustSec states that no patched `rsa` release exists. `russh 0.62.5` is current and pins the affected RSA release exactly. |
| `RUSTSEC-2019-0036` | vendored `libsignal-protocol 0.1.0 -> failure 0.1.8` | `failure` type-confusion advisory. The wrapper derives `Fail` rather than manually overriding the private type-ID method, which RustSec says is not affected, but the obsolete dependency remains in shipped OMEMO code. | No safe `failure` upgrade; requires replacing/forking the vendored wrapper's error API. |
| `RUSTSEC-2020-0036` | same vendored Signal chain | `failure` is end-of-life and unmaintained. | No safe upgrade; same source migration as above. |

The main lockfile already contains the patched `rustls-webpki 0.103.13` for
modern TLS consumers. The vulnerable `0.102.8` copy remains solely because the
libSQL 0.6 stack uses `rustls 0.22`; `cargo tree --invert` confirmed that path.

## Why the Obvious Parent Upgrades Do Not Clear Findings

### libSQL

Stable `libsql 0.9.30` is available, but current crate metadata still requests
`hyper-rustls 0.25`, which in turn uses `rustls 0.22` and
`rustls-webpki 0.102`. The latest published `0.10.0-pre.4` metadata retains the
same legacy HTTP/TLS dependency line. Therefore even a large `libsql 0.6 -> 0.9`
migration does not clear these four advisories today.

Recommended follow-up: keep PostgreSQL as the preferred deployment backend;
for libSQL users, avoid remote/TLS mode where operationally possible. Recheck
when libSQL moves its `hyper-rustls` dependency to a line using
`rustls-webpki >=0.103.13`.

### russh / RSA

`russh 0.62.5` is the current stable release and pins `rsa 0.10.0-rc.18`.
Removing LunarWing's explicit `rsa` feature would remove RSA key parsing and
signing, but that is a behavior/security-policy change rather than a crate
upgrade: RSA credentials are currently accepted by the shared agent used by
workers and `ssh_git`. It needs a dedicated migration with configuration
validation and tests, not a release-audit lockfile edit.

Recommended follow-up: either reject RSA keys consistently at ingestion and in
the shared agent, then remove the `russh/rsa` feature, or migrate once a patched
upstream RSA implementation is available. Existing operators should use
Ed25519 or ECDSA keys.

### vendored Signal wrapper

The vendored `libsignal-protocol 0.1.0` wrapper directly depends on
`failure 0.1.8`; crates.io has no newer wrapper release. Replacing `failure`
requires editing the vendored API or replacing the legacy
`libsignal-protocol-c` binding, with OMEMO serialization and interoperability
tests.

Recommended follow-up: schedule a dedicated Signal/OMEMO dependency migration.
Do not perform a mechanical error-type replacement in a release checklist item.

## Vendored and Feature-Only Note

Scanning `ic/vendor/libsignal-protocol/Cargo.lock` with all features also
reported `RUSTSEC-2021-0134` (`rental` is unmaintained). It is not present in
the shipped LunarWing graph: `rental` is optional and only enabled by the
vendor crate's `crypto-openssl` feature, while LunarWing uses the default
`crypto-native` feature. The exception therefore does not belong in
`ic/deny.toml`, but the finding reinforces the recommendation to replace the
legacy wrapper rather than enable its OpenSSL backend.

## Clean Graphs

With all features enabled, no advisory was found in the other tracked roots:
DarkIRC, both socket clients, both Gotify copies, Multica, XMPP channel,
OpenClaw XMPP bridge, test echo tool, WeeChat relay, Pebble, Pebble integration,
OCR sidecar, voice sidecar, and the duplicate REPL client.

The excluded/lockless roots also produced no additional advisory. The main fuzz
root reproduced the same `rsa` and `rustls-webpki` findings through its path
dependency on LunarWing; it did not introduce a fuzz-only vulnerability.

## Release Recommendation

1. Do not add or remove an advisory exception for v2.0.7.0.
2. Do not claim that `cargo-deny` is clean without noting that seven findings
   are explicitly accepted by policy.
3. Keep the RSA, libSQL TLS, and vendored Signal migrations as separate,
   security-reviewed work.
4. Re-run the CI-pinned advisory check after the release lockfile reconciliation
   and before tagging, because RustSec data can change independently of source.
