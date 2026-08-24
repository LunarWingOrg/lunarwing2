# Crate Audit Status Report - v2.0.5.0

**Date:** 2026-08-05
**Scope:** LunarWing v2.0.5.0, all project Cargo manifests and 18 tracked lockfiles
**Supersedes:** `CRATE_AUDIT_v2.0.4.0.md` (2026-07-30)
**Branch:** `crate1/cmc/v2.0.5.0` (release checklist items 2, 3, and 5)

---

## Summary

- All ten LunarWing-owned release packages are at `2.0.5`, including the four
  WASM channels and `xmpp-bridge`.
- Item 1 already refreshed every tracked lockfile. This audit found and applied
  two additional compatible transitive patches that became available in the
  main and bridge graphs: `android_system_properties` 0.1.5 -> 0.1.6 and
  `keccak` 0.2.0 -> 0.2.1.
- `pdf-extract` 0.7.12 -> 0.12.0 was the only remaining update that should block
  release. It moves `lopdf` 0.34.0 -> 0.42.0 and fixes the untrusted-PDF
  stack-overflow denial of service in `RUSTSEC-2026-0187`.
- A post-update `cargo update --dry-run --verbose` sweep covered all 17
  non-vendored lockfile roots. Every root reported `Locking 0 packages`; no
  semver-compatible lockfile update remains.
- Forty-two unique direct dependency names still have newer stable versions,
  but every remaining move crosses a Cargo manifest compatibility boundary.
  They are listed below with explicit migration scope and deferral rationale.
- GitLab CI contains no pinned external Rust crate versions. Dependency
  resolution comes from manifests and lockfiles, so no CI version edit is
  required.

---

## Scope and Method

The audit covered:

- `ic/Cargo.toml`, its four workspace crates, and the standalone XMPP bridge.
- All tracked channel, tool, OCR, Pebble, OpenClaw bridge, fixture, and socket
  client lockfile roots used by item 1.
- Standalone manifests without tracked lockfiles, including WASM tools,
  `job_prompt`, fuzz harnesses, and socket REPL copies.
- The tracked `ic/kCargo.toml` shadow manifest, which now mirrors the
  `pdf-extract = "0.12"` security constraint.

`ic/vendor/libsignal-protocol/Cargo.lock` remains excluded from lockfile
mutation because it is vendored upstream source. Its known advisory exceptions
remain tracked in `deny.toml`.

Version evidence came from Cargo's crates.io index on 2026-08-05. Each active
lockfile root was checked with:

```bash
taskset -c 0-5 cargo update --manifest-path <Cargo.toml> --dry-run --verbose
```

The main graph reports 43 unchanged package entries behind latest. That count
includes transitive and vendored entries such as `cfg-if` 0.1, `generic-array`
0.14, and `agent-client-protocol-schema`; it is not a count of direct manifest
dependencies. The sections below consolidate direct dependencies by crate name
and migration.

---

## Applied in This Audit

| Crate or policy | Before | After | Reason |
|---|---:|---:|---|
| `pdf-extract` | 0.7.12 | 0.12.0 | Latest stable line; public `extract_text_from_mem(&[u8])` API used by LunarWing is unchanged. |
| `lopdf` | 0.34.0 | 0.42.0 | Fixes `RUSTSEC-2026-0187`; patched versions are `>=0.42.0`. |
| `android_system_properties` | 0.1.5 | 0.1.6 | Compatible transitive patch found by the final update sweep. |
| `keccak` | 0.2.0 | 0.2.1 | Compatible transitive patch found by the final update sweep. |
| `deny.toml` | Advisory ignored | Ignore removed | The locked graph no longer contains vulnerable `lopdf` 0.34. |

Both `ic/Cargo.lock` and `ic/bridges/xmpp-bridge/Cargo.lock` were refreshed.
The bridge lock mirrors the main package through its path dependency.

### Security impact

LunarWing parses user-supplied PDF bytes through
`pdf_extract::extract_text_from_mem`. `lopdf` through 0.41 recursively parsed
nested PDF objects without a depth bound; a small crafted document could abort
the process with stack overflow. `lopdf` 0.42 adds the required bound and returns
an error instead. This is a direct availability fix for an exposed document
ingestion path, not dependency churn for its own sake.

---

## LunarWing-Owned Versions

| Package group | Version | Manifests |
|---|---:|---|
| Main daemon | 2.0.5 | `ic/Cargo.toml` |
| Common, engine, safety, skills | 2.0.5 | `ic/crates/*/Cargo.toml` |
| XMPP bridge | 2.0.5 | `ic/bridges/xmpp-bridge/Cargo.toml` |
| XMPP and Multica channels | 2.0.5 | `ic/channels-src/{xmpp,multica}/Cargo.toml` |
| DarkIRC channel | 2.0.5 | `darkirc_channel_for_lunarwing/darkirc/Cargo.toml` |
| WeeChat channel | 2.0.5 | `lunarwing_weechat_wss/weechat_relay/Cargo.toml` |

WASM tools retain independent package versions. Their differing versions are
intentional and are not LunarWing release-version drift.

---

## Remaining Direct Dependency Migrations

No entry in this section can be applied by a compatible `cargo update`. Each
requires a manifest edit and focused migration testing.

### High-value dedicated follow-ups

| Crate or group | Locked -> latest stable | Recommendation |
|---|---|---|
| `wasmtime`, `wasmtime-wasi`, `wasmparser` | 36.0.13 -> 47.0.3; 0.220.1 -> 0.255.0 | Update together in a dedicated sandbox/WASI migration. Eleven Wasmtime majors cross core extension security and component-model APIs. Current 36.0.13 already contains the fix required by `RUSTSEC-2026-0222`, so this is important maintenance, not an open release blocker. |
| `reqwest` | 0.12.28 -> 0.13.4 | Dedicated HTTP/TLS migration across daemon, skills, Pebble, and OCR. The 0.13 feature and TLS-provider changes require outbound proxy, redirect, multipart, and native-root tests. It can remove the direct 0.12 copy because `rig-core` already brings 0.13. |
| RustCrypto/key crates | `aes-gcm` 0.10.3 -> 0.11.0; `ed25519-dalek` 2.2.0 -> 3.0.0; `hkdf`/`hmac` 0.12 -> 0.13; `sha2` 0.10.9 -> 0.11.0; `x25519-dalek` 2.0.1 -> 3.0.0 | Migrate as a tested crypto set. Several newer lines already exist transitively, but persisted-secret compatibility, OMEMO vectors, key exchange, and signature behavior make piecemeal changes unsafe. |
| `tokio-tungstenite` | 0.26.2 -> 0.30.0 | Coordinate daemon and `pebble4lunarwing` WebSocket migrations. Test gateway handshake rejection, TLS, ping/pong, close frames, and byte/text message conversions. |
| `libsql` | 0.6.0 -> 0.9.30 | Keep deferred to a DB migration branch. Contrary to the prior audit's expectation, 0.9.30 still requests `hyper-rustls` 0.25, `tonic` 0.11, `tower-http` 0.4.4, `base64` 0.21, and `bincode` 1; it does not by itself clear the existing legacy advisory cluster. PostgreSQL remains the primary backend. |
| `wit-bindgen` guests | 0.36/0.41 -> 0.60.0 | Upgrade only with host WIT regeneration and component compatibility tests. Current guest versions are intentionally coupled to channel/tool interface generation. |

### Narrow satellite follow-ups

These look mechanically small, but still cross pre-1.0 or major compatibility
boundaries and should land with their owning component tests:

| Crate | Locked -> latest stable | Scope |
|---|---|---|
| `base64` | 0.22.1 -> 0.23.1 | Daemon, XMPP, vision, and OCR encoding/decoding vectors. |
| `crossterm` | 0.28.1 -> 0.29.0 | Pebble terminal event and style handling; daemon is already on 0.29. |
| `getrandom` | 0.3.4 -> 0.4.3 | Pebble runtime random-fill path. |
| `governor` | 0.8.1 -> 0.10.4 | OCR direct rate limiter and quota behavior. |
| `sha2` | 0.10.9 -> 0.11.0 | Pebble and OCR checksum/cache-key fixtures; coordinate with the main crypto set. |
| `signal-hook` | 0.3.18 -> 0.4.4 | Pebble interrupt registration and cleanup. |
| `thiserror` | 1.0.69 -> 2.0.19 | OCR derive-only use; main workspace is already on 2.x. |
| `warp` | 0.3.7 -> 0.4.3 | OCR server; Hyper 1 migration and new `server` feature require route/rejection tests. |

### Deferred long tail

| Crate or group | Locked -> latest stable | Why deferred |
|---|---|---|
| `agent-client-protocol` | 0.10.4 -> 2.0.0 | Large SDK/schema API jump in agent runtime path. |
| `bollard` | 0.18.1 -> 0.21.0 | Docker sandbox models and options changed; container boundary is security-sensitive. |
| `cap-fs-ext`, `cap-std` | 3.4.5 -> 4.0.2 | Capability types and filesystem APIs must move together. |
| `criterion` | 0.5.1 -> 0.8.2 | Benchmark-only; no runtime value before benchmark maintenance. |
| `cron` | 0.13.0 -> 0.17.0 | Routine schedule parsing and semantics need behavior tests. |
| `fs4` | 0.6.6 -> 1.1.0 | File-lock correctness and trait API migration. |
| `html-to-markdown-rs` | 2.30.0 -> 3.10.5 | Untrusted HTML conversion behavior changed; needs corpus comparison. |
| `json5` | 0.4.1 -> 1.3.1 | Import parser and error behavior; low current value. |
| `lru` | 0.16.4 -> 0.18.2 | Small cache migration with no security or deduplication leverage. |
| `nix` | 0.30.1 -> 0.31.3 | Feature and Unix API changes; other dependency lines remain. |
| `rand` | 0.8.7 -> 0.10.2 | Breaking RNG API redesign; lockfile already carries 0.9 and 0.10 transitively. |
| `refinery` | 0.8.16 -> 0.9.2 | PostgreSQL migration execution path needs DB integration tests. |
| `rig-core` | 0.40.0 -> 0.41.0 | Pre-1.0 LLM abstraction update; newly released and broad provider impact. |
| `rustyline` | 17.0.2/15.0.0 -> 18.0.1 | Main CLI and Pebble terminal integrations require separate compatibility work. |
| `secret-service`, `zbus` | 4.0.0 -> 5.1.0; 4.4.0 -> 5.18.0 | Keyring and D-Bus async APIs should move together. |
| `termimad` | 0.34.1 -> 0.35.1 | Rendering-only update with low release value. |
| `tokio-postgres-rustls` | 0.13.0 -> 0.14.0 | Database TLS connector migration needs certificate and connection tests. |
| `toml` | 0.8.23 -> 1.1.4+spec-1.1.0 | Config and secret-reinjection parser behavior needs compatibility fixtures. |
| `tower-http` | 0.6.11 -> 0.7.0 | Middleware API migration; 0.6 remains current and 0.4 remains through libSQL. |
| `zip` | 2.4.2 -> 8.6.0 | Six-major archive parser jump on untrusted extension/document inputs. |

---

## Advisory Status

`RUSTSEC-2026-0187` is cleared from both active lockfiles and removed from
`deny.toml`. Remaining ignores are unchanged:

- `RUSTSEC-2026-0049`, `RUSTSEC-2026-0098`, `RUSTSEC-2026-0099`, and
  `RUSTSEC-2026-0104`: legacy `rustls-webpki` chain retained by libSQL.
- `RUSTSEC-2023-0071`: RSA Marvin timing advisory without an upstream
  constant-time release.
- `RUSTSEC-2019-0036` and `RUSTSEC-2020-0036`: vendored
  `libsignal-protocol`/`failure` chain without an upstream patch.

`cargo-deny` is not installed on this VM. Release checklist item 6 owns the
full live advisory/license scan and review of the remaining ignores. This audit
verified the removed advisory directly against the RustSec patched range and
the resolved lock graph.

---

## CI/CD Review

- `.gitlab/` has no hardcoded external Rust crate versions.
- Cargo manifests and committed lockfiles remain the dependency source of
  truth.
- No CI image, installer, or policy edit is needed for the updates in this
  audit.

---

## Verification

- All 17 active lockfile roots: `cargo update --dry-run --verbose` reports
  `Locking 0 packages` after the applied updates.
- `pdf-extract` 0.12.0 source confirms the unchanged
  `extract_text_from_mem(&[u8]) -> Result<String, OutputError>` API and a direct
  `lopdf = "0.42"` constraint.
- `taskset -c 0-5 cargo check -j6 --locked`: pass.
- `taskset -c 0-5 cargo check -j6 --all-features --locked`: pass.
- `taskset -c 0-5 cargo test -j6 document_extraction::tests::extracts_pdf_text
  -- --exact --nocapture --test-threads=6`: pass, one test.
- `taskset -c 0-5 cargo check -j6 --manifest-path
  bridges/xmpp-bridge/Cargo.toml --locked`: pass.
- `taskset -c 0-5 cargo fmt --all -- --check`: pass.
- `python3 .gitlab/scripts/validate_config.py`: pass, eight GitLab CI files and
  local helper references validated.
- `cargo tree --locked --invert lopdf@0.42.0` confirms the only active path is
  `lopdf 0.42.0 -> pdf-extract 0.12.0 -> lunarwing 2.0.5`, plus the expected
  path onward to `xmpp-bridge` in its standalone lockfile.

---

## Conclusion

Checklist items 2 and 5 are satisfied at the compatible-update boundary: every
active lockfile is current within its manifest constraints, all owned release
crates are at 2.0.5, and the one security-relevant direct migration is applied.
Checklist item 3 is satisfied by this document: every remaining older direct
line is explicit, prioritized, and assigned a migration rationale rather than
being silently stale.
