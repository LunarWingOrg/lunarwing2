# Crate Security Audit - v2.0.7.0

**Date:** 2026-08-10

**Reconciliation update:** 2026-08-11, based on `d7a7ae1c76b2`

**Source commit:** `c157a53374fa`

**Scope:** all 19 tracked Cargo lockfiles, 12 excluded/lockless Cargo roots,
and the ignored local GitLab tool lockfile

**Reference:** `CRATE_AUDIT_v2.0.5.0.md`

## Conclusion

The original live RustSec scan found the same seven accepted advisories in the
main shipped LunarWing graph that are already listed in `ic/deny.toml`; no
unlisted advisory ID was found in any root. The standalone XMPP bridge
reproduces five of those IDs through its LunarWing path dependency; the other
active lockfile roots were clean.

Post-audit release reconciliation applied the compatible main-lockfile updates
and migrated LunarWing's two direct `reqwest` declarations from 0.12 to 0.13.
This removed the duplicate `reqwest 0.12.28` graph while preserving required
query, form, multipart, streaming, JSON, and TLS features. These maintenance
changes compile and pass focused tests, but do not clear any documented
accepted advisory or alter advisory policy.

The seven shipped-graph findings are not solved by a compatible lockfile update:

- four `rustls-webpki` findings are pinned behind `libsql`'s legacy TLS stack;
- the `rsa` Marvin timing advisory has no patched RustCrypto release and is
  enabled through `russh`;
- two `failure` findings come from the vendored Signal wrapper and have no safe
  upstream upgrade.

These should remain tracked, but none justifies an untested backend or crypto
migration immediately before v2.0.7.0. No `deny.toml` change is recommended.

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
temporary lockfiles were removed afterward. Including the ignored local
GitLab tool lockfile, this covered 32 Cargo roots. The stale standalone XMPP
bridge lock could not be read with `--locked` after its path dependency changed,
so its currently resolved graph was scanned without `--locked` and its lockfile
was restored byte-for-byte.

A `cargo update --dry-run --verbose` of the main graph was also reviewed. It
found compatible maintenance updates, but none affects the advisory set. Those
ordinary updates were subsequently applied during the 2026-08-11 release
lockfile reconciliation described below.

## Post-Audit Dependency Reconciliation - 2026-08-11

### Applied changes

`taskset -c 0-5 cargo update --ignore-rust-version --verbose` locked 44 package
changes to their latest manifest-compatible versions. Notable changes include:

- `async-trait 0.1.91 -> 0.1.92`;
- `aws-lc-rs 1.17.3 -> 1.18.0` and `aws-lc-sys 0.43.0 -> 0.44.0`;
- the Futures family `0.3.33 -> 0.3.34`;
- `moka 0.12.15 -> 0.12.16`, `portable-atomic 1.14.0 -> 1.15.0`, and
  `serde_with 3.21.0 -> 3.22.0`;
- `readabilityrs 0.1.3 -> 0.1.4`;
- `russh 0.62.5 -> 0.62.6`;
- the modern `rustls-webpki 0.103.13 -> 0.103.14` copy;
- `thiserror 2.0.19 -> 2.0.20`;
- compatible WebAssembly support updates including `wasm-bindgen 0.2.126 ->
  0.2.127` and the transitive `wasm-encoder`, `wasmparser`, `wast`, and `wat`
  255 lines to 256.

The update also removed obsolete transitive copies including `crc 3.4.0`,
`crc-catalog 2.5.0`, `kuchikikiki 0.9.1`, and `wasm-streams 0.4.2`. New
transitive packages include the `jiff 0.2` family and `defmt 1.1` family.

Both direct `reqwest` declarations now use 0.13. The daemon enables `json`,
`multipart`, `query`, `form`, `charset`, `http2`, `rustls`, and `stream`;
`lunarwing_skills` enables `json`, `multipart`, `query`, and `rustls`. These
features are explicit because reqwest 0.13 makes query and form support
optional, replaces the old `rustls-tls-native-roots` feature with `rustls`, and
uses the platform verifier for system trust. Source review found six daemon
`.form(...)` call sites, a daemon HTTP-tool `.query(...)` call, four
skill-catalog `.query(...)` calls across the two catalog implementations, and
catalog multipart publication.

Cargo feature unification exposed one compatibility issue during review:
`rig-core`'s aggregate `reqwest` feature bundles `charset`, `http2`, and
`system-proxy`. Versioned reqwest 0.12.28 and 0.13.4 source confirms that both
versions honor `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY` without
that feature; `system-proxy` adds Windows and macOS OS-level proxy discovery.
Using `.no_proxy()` would therefore have broken existing environment-proxy
deployments rather than preserved behavior.

LunarWing now enables shared `charset` and `http2` features directly and gives
Rig only its `rustls` feature. Rig's reqwest dependency is mandatory, so provider
support remains compiled; the resolved feature tree confirms `system-proxy` and
`hyper-util/client-proxy-system` are absent. Direct clients retain existing
environment-proxy behavior and do not gain OS proxy discovery. Rig retains
charset and HTTP/2 support but likewise uses environment proxies instead of
Windows/macOS OS settings. Deduplication necessarily shares charset and HTTP/2
with direct clients, which may now negotiate HTTP/2 and apply charset decoding.

The resolved main graph now has one `reqwest 0.13.4` copy shared by LunarWing,
`lunarwing_skills`, and `rig-core 0.40.0`; `reqwest 0.12.28` is gone. Modern
HTTP consumers resolve through `hyper-rustls 0.27.9`, `rustls 0.23.43`,
`rustls-platform-verifier 0.7.0`, and `rustls-webpki 0.103.14`.

### Verification

All commands ran from `ic/` with six CPUs and the host-Rust compatibility
override required by this development machine:

```text
taskset -c 0-5 cargo check -j6 --locked --all-features --ignore-rust-version
taskset -c 0-5 cargo test -j6 --locked --ignore-rust-version -p lunarwing_skills --all-features -- --test-threads=6
taskset -c 0-5 cargo test -j6 --locked --ignore-rust-version --all-features --lib tools::builtin::http::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --locked --ignore-rust-version --all-features --lib cli::oauth_defaults::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --locked --ignore-rust-version --all-features --lib tools::mcp::auth::tests -- --test-threads=6
taskset -c 0-5 cargo clippy -j6 --locked --all --benches --tests --examples --all-features --ignore-rust-version -- -D warnings
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo tree --locked -e features --invert reqwest@0.13.4
```

Results:

- all-feature compile check passed;
- `lunarwing_skills`: 170 passed;
- built-in HTTP tool: 47 passed, 1 pre-existing network test ignored;
- CLI OAuth defaults: 34 passed;
- MCP auth: 46 passed;
- all-feature Clippy passed with zero warnings;
- feature-tree review found one reqwest copy and no `system-proxy` activation;
- formatting and `git diff --check` passed.

The CI-pinned `cargo-deny` binary used for the original audit was not present in
this reconciliation environment, so this update does not claim a fresh RustSec
database scan. Locked inverse-tree checks confirm that the seven documented
paths retain the same security disposition. CI must rerun its pinned advisory
check before tagging.

### Remaining update backlog

| Priority | Crate set | Current -> candidate | Recommendation |
|---|---|---|---|
| Next deduplication | `tokio-tungstenite` | 0.26.2 -> 0.29.0 | Target 0.29 first because Axum already brings 0.29; this removes a duplicate. Do not jump directly to 0.30 while Axum remains on 0.29. Test worker reconnect, TLS, handshake rejection, ping/pong, close frames, and byte/text conversions. Versions 0.29 and 0.30 require Rust 1.63 and 1.85 respectively, both below LunarWing's MSRV. |
| Next provider maintenance | `rig-core` | 0.40.0 -> 0.41.0 | Small version step but pre-1.0 and broad LLM-provider impact. Test provider requests, streaming, tool calls, and error mapping. |
| Small isolated updates | sidecar and Pebble crates | OCR `thiserror 1 -> 2`, OCR/voice `governor 0.8 -> 0.10`, Pebble `crossterm 0.28 -> 0.29`, `getrandom 0.3 -> 0.4`, `signal-hook 0.3 -> 0.4`, vision extension `base64 0.22 -> 0.23` | Apply per independent Cargo root with focused tests. Main LunarWing already uses `thiserror 2`, `crossterm 0.29`, and `base64 0.23`. |
| Dedicated sandbox migration | `wasmtime`, `wasmtime-wasi`, direct `wasmparser` | 36.0.13 -> 47.0.3; 0.220.1 -> 0.256.0 | Upgrade together. Wasmtime 47 requires Rust 1.94, compatible with project MSRV 1.96.1, but eleven major releases cross component-model and sandbox APIs. Current 36.0.13 already fixes `RUSTSEC-2026-0222`, so this is maintenance rather than a release blocker. |
| Dedicated crypto migration | RustCrypto/key crates | `aes-gcm 0.10.3 -> 0.11.0`, `ed25519-dalek 2.2.0 -> 3.0.0`, `hkdf`/`hmac 0.12 -> 0.13`, `sha2 0.10.9 -> 0.11.0`, `x25519-dalek 2.0.1 -> 3.0.0` | Migrate as a tested set. Newer lines already exist transitively, but persisted ciphertext, OMEMO vectors, signatures, and key exchange require compatibility testing. `aes-gcm 0.11` requires Rust 1.85. |
| Low deduplication value | `rand` | direct 0.8.7 -> 0.10.2 | Defer until parent crates move. Numerous dependencies still require 0.8, so changing only the direct declaration does not remove that line. |
| Low release value | `html-to-markdown-rs` | resolved 2.30.0 -> 3.10.6 | Feature-gated major API migration with no identified security payoff. Handle separately. |
| Database migration only | `libsql` | 0.6.0 -> stable 0.9.30; latest 0.10.0-pre.4 | Defer. Neither candidate removes the legacy `hyper-rustls 0.25` / `rustls 0.22` advisory chain, and 0.10 is prerelease. PostgreSQL remains preferred. |

## Active Shipped-Graph Findings

| Advisory | Locked path | Security assessment | Upgrade status |
|---|---|---|---|
| `RUSTSEC-2026-0049` | `libsql 0.6.0 -> hyper-rustls 0.25.0 -> rustls 0.22.4 -> rustls-webpki 0.102.8` | Incorrect CRL distribution-point handling. Relevant only to the optional libSQL remote/TLS path. | Patched in `rustls-webpki >=0.103.10`, but `rustls 0.22` requires the 0.102 line. |
| `RUSTSEC-2026-0098` | same libSQL chain | URI name constraints can be accepted incorrectly after certificate signature verification. | Patched in the 0.103 line; blocked by the same parent constraints. |
| `RUSTSEC-2026-0099` | same libSQL chain | Wildcard certificates can bypass a permitted-subtree name constraint after signature verification. | Patched in the 0.103 line; blocked by the same parent constraints. |
| `RUSTSEC-2026-0104` | same libSQL chain | Reachable panic while parsing a CRL, but applications that do not use CRLs are unaffected. | Patched in `rustls-webpki >=0.103.13`; blocked by the same parent constraints. |
| `RUSTSEC-2023-0071` | `russh 0.62.6 -> rsa 0.10.0-rc.18` (also through `ssh-key`) | Network-observable RSA timing side channel. The built-in in-process SSH executor rejects RSA private keys, but the shared `russh` SSH-agent path can still parse and sign with configured RSA keys for workers/`ssh_git`; therefore the exception is not purely dead code. | RustSec states that no patched `rsa` release exists. `russh 0.62.6` pins the affected RSA release exactly. |
| `RUSTSEC-2019-0036` | vendored `libsignal-protocol 0.1.0 -> failure 0.1.8` | `failure` type-confusion advisory. The wrapper derives `Fail` rather than manually overriding the private type-ID method, which RustSec says is not affected, but the obsolete dependency remains in shipped OMEMO code. | No safe `failure` upgrade; requires replacing/forking the vendored wrapper's error API. |
| `RUSTSEC-2020-0036` | same vendored Signal chain | `failure` is end-of-life and unmaintained. | No safe upgrade; same source migration as above. |

The main lockfile contains the patched `rustls-webpki 0.103.14` for
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

`russh 0.62.6` pins `rsa 0.10.0-rc.18`.
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
4. Re-run the CI-pinned advisory check after this completed lockfile
   reconciliation and before tagging, because RustSec data can change
   independently of source.
