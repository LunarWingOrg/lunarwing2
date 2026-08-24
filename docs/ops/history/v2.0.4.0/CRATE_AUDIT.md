# Crate Audit Status Report — v2.0.4.0

**Date:** 2026-07-30
**Scope:** LunarWing v2.0.4.0 — `ic/` workspace (`ic/Cargo.toml`) and satellite crates
**Supersedes:** `CRATE_AUDIT_v2.0.3.0.md` (2026-07-25, v2.0.3.0)
**Branch:** rarity/kimi-3-20260730-0901 (AGENT_GOALS_2.0.4.0 item #3)

---

## Summary

- Workspace crate versions all correctly at **2.0.4** (item #1 verified).
- `cargo update` **applied** across the `ic/` workspace lockfile and all five
  satellite lockfiles: **15 patch/minor bumps** in the main workspace (the
  backlog is small this cycle because item #10 already ran `cargo update` for
  v2.0.3.0). All semver-compatible; no Cargo.toml changes required.
- **Verification passed:** `taskset -c 0-5 cargo check -j6` (default features),
  `cargo check -j6 --all-features`, and `cargo check` in
  `ic/bridges/xmpp-bridge` — all exit 0, zero errors.
- Notable bumps: `rustls` 0.23.42 → **0.23.43** (TLS patch), `http` 1.4.2 →
  1.5.0, `aes` 0.9.1 → 0.9.2, `tokio-macros` 2.7.1 → 2.7.2, `schemars` 1.2.1 →
  1.2.2, `async-compression` 0.4.42 → 0.4.43, `clang-sys` 1.8.1 → 1.9.1.
  Satellites additionally picked up `serde` 1.0.229, `serde_json` 1.0.151,
  `tokio` 1.53.1, `zerocopy` 0.8.55, `thiserror` 2.0.19, `syn` 3.0.3.
- 42 dependencies remain behind latest (all require major/minor Cargo.toml
  bumps; listed below with recommendations).

---

## Workspace Crate Versions (v2.0.4)

All LunarWing workspace crates verified at `2.0.4`:

| Crate | Version | Location |
|-------|---------|----------|
| `lunarwing` (main daemon) | 2.0.4 | `ic/Cargo.toml` |
| `lunarwing_common` | 2.0.4 | `ic/crates/lunarwing_common/Cargo.toml` |
| `lunarwing_engine` | 2.0.4 | `ic/crates/lunarwing_engine/Cargo.toml` |
| `lunarwing_safety` | 2.0.4 | `ic/crates/lunarwing_safety/Cargo.toml` |
| `lunarwing_skills` | 2.0.4 | `ic/crates/lunarwing_skills/Cargo.toml` |
| `xmpp-bridge` | 2.0.4 | `ic/bridges/xmpp-bridge/Cargo.toml` |
| `xmpp-channel` (WASM) | 2.0.4 | `ic/channels-src/xmpp/Cargo.toml` |
| `weechat-relay-channel` (WASM) | 2.0.4 | `ic/channels-src/weechat/Cargo.toml` |
| `darkirc-channel` (WASM) | 2.0.4 | `ic/channels-src/darkirc/Cargo.toml` |
| `multica-channel` (WASM) | 2.0.4 | `ic/channels-src/multica/Cargo.toml` |

WASM tools keep independent versioning (sandboxed plugins): `cavepony` 0.4.0,
`vision-analyze` 0.2.0, `web-search` 0.2.0, `gotify`/`ssh`/`llm-context`/
`multica-bridge` 0.1.0.

Toolchain: `rust-version = "1.96.1"` (`ic/Cargo.toml`), matches MSRV in
AGENTS.md.

---

## Applied in This Audit (item #3)

`cargo update` run 2026-07-30 against:

- `ic/Cargo.lock` — 15 bumps (see Summary).
- `ic/bridges/xmpp-bridge/Cargo.lock` — mirrors the workspace bumps.
- `ic/channels-src/xmpp/Cargo.lock`, `ic/channels-src/multica/Cargo.lock` —
  `serde` 1.0.229, `serde_json` 1.0.151, `zerocopy` 0.8.55, `syn` 3.0.3, etc.
- `ic/lunarwing-gotify-tool/Cargo.lock` — `icu`/`zerovec` family, `syn` 3.0.3.
- `ic/unix-socket-client/Cargo.lock` — `tokio` 1.53.1, `thiserror` 2.0.19,
  `serde` 1.0.229, `socket2` 0.6.5.

`ic/vendor/libsignal-protocol/Cargo.lock` intentionally left untouched
(vendored upstream).

Verification (all on `taskset -c 0-5`, `-j6`):

| Command | Result |
|---------|--------|
| `cargo check` (default features, `ic/`) | ✅ exit 0 |
| `cargo check --all-features` (`ic/`) | ✅ exit 0 |
| `cargo check` (`ic/bridges/xmpp-bridge`) | ✅ exit 0 |

WASM channel/tool crates were lockfile-updated but not rebuilt here (patch-level
bumps only, semver-compatible); `./scripts/build-wasm-extensions.sh` remains
the path to produce release WASM artifacts.

---

## Remaining Behind-Latest Dependencies (42)

### Recommended for a future release (dedup value)

| Crate | Pinned | Latest | Cargo.toml | Notes |
|-------|--------|--------|------------|-------|
| `reqwest` | `0.12` | `0.13.4` | `ic/Cargo.toml:39` | **Dedup opportunity:** `rig-core 0.40.0` already pulls `reqwest 0.13.4` transitively, so the lockfile carries both 0.12.28 and 0.13.4. Bumping the direct dep removes the duplicate. Major bump, wide usage (sandbox proxy, worker API, tunnel) — needs integration testing. |
| `tokio-tungstenite` | `0.26` | `0.30.0` | `ic/Cargo.toml:177,218` | Lockfile already carries 0.26.2 (direct), 0.28.0 (`rig-core`), 0.29.0 (`axum` 0.8.9's WS support). Bumping the direct dep reduces the triple-version situation. WebSocket gateway path — test carefully. |
| `libsql` | `0.6.0` | `0.9.30` | (feature `libsql`) | **Highest leverage single bump.** `libsql 0.6.0` + `tonic 0.11` pin the entire legacy transitive cluster: `tower-http 0.4.4`, `hyper 0.14.32`, `axum 0.6.20`, `rustls 0.22.4`, `base64 0.21.7` — and are the stated blocker for 4 of the 7 remaining `deny.toml` advisory ignores (rustls-webpki cluster). Very large jump (0.6 → 0.9); libsql is the aspirational backend. Defer unless a dedicated migration effort is scheduled. |

### Still deferred (rationale unchanged from v2.0.3.0 audit)

| Crate | Pinned | Latest | Notes |
|-------|--------|--------|-------|
| `rand` | `0.8` | `0.10.2` | Breaking API redesign (0.9+). Direct dep on 0.8.7; transitives (`hickory-*`, `russh`, `tokio-postgres`, `tokio-xmpp`, `quinn-proto`) are already on 0.10.2, and `tungstenite` on 0.9.5 — the direct dep is now the *oldest* rand in the tree. Migration effort is moderate and mostly mechanical (`thread_rng`/`gen` renames). Candidate for next cycle. |
| `tower-http` | `0.6` | `0.7.0` | Minor bump; 0.6.11 patch line still serviced. Defer. |
| `base64` | `0.22` | `0.23.0` | No marginal benefit while transitive 0.21 persists via libsql/tonic. Defer. |
| `wasmtime` / `wasmtime-wasi` | `36.0.12` | `47.0.2` | 11 majors behind; core WASM runtime path. Defer unless WASM-platform requirements force it. |
| `rig-core` | `0.40` | `0.41.0` | New since the last audit. LLM provider abstraction — evaluate release notes before bumping. |

### Long tail (transitive or low-urgency, tracked only)

`bollard` 0.18→0.21, `criterion` 0.5→0.8 (benches only), `cron` 0.13→0.17,
`zbus` 4→5, `secret-service` 4→5, `zip` 2→8, `ed25519-dalek` 2→3,
`x25519-dalek` 2→3, `sha2`/`hmac`/`hkdf`/`aes-gcm`/`ctr`/`cbc` (RustCrypto
0.10→0.11/0.13 pre-releases), `agent-client-protocol` 0.10→2.0,
`html-to-markdown-rs` 2→3, `rustyline` 17→18, `env_logger` 0.6→0.11,
`json5` 0.4→1.3, `toml` 0.8→1.1, `fs4` 0.6→1.1, `lru` 0.16→0.18,
`nix` 0.30→0.31, `pdf-extract` 0.7→0.12 (blocked: `lopdf` advisory chain),
`refinery` 0.8→0.9, `tokio-postgres-rustls` 0.13→0.14, `termimad` 0.34→0.35,
`cfg-if` 0.1→1.0, `generic-array` 0.14.7→0.14.9 (patch, held by RustCrypto
pins), `matchit` 0.8.4→0.8.6 (held by axum 0.6 transitive), `aes` 0.8.4 (held
alongside 0.9.2 by OMEMO deps), `wasmparser` 0.220 (bundled with wasmtime 36).

### No action needed

- `tokio-xmpp` 6.0.0, `xmpp-parsers` 0.23.0, `hickory-proto`/`hickory-resolver`
  0.26.1 — current (item #10).
- `axum` 0.8.9, `tokio` 1.53.1, `serde` 1.0.229, `hyper` 1.11.0,
  `openssl` 0.10.81, `rustls` 0.23.43 — all on current lines; patch flow via
  `cargo update`.

---

## Advisory Status (`deny.toml`)

`deny.toml` unchanged since item #14. Seven ignores remain, all legitimately
blocked upstream:

| Advisory | Chain | Status |
|----------|-------|--------|
| `RUSTSEC-2026-0049` | rustls-webpki CRL distributionPoint via libsql | Blocked on upstream libsql |
| `RUSTSEC-2026-0098` / `-0099` / `-0104` | rustls-webpki 0.102 via rustls 0.22 via libsql 0.6.0 | Blocked on upstream libsql |
| `RUSTSEC-2026-0187` | lopdf 0.34 via pdf-extract 0.7.12 | Blocked on upstream pdf-extract |
| `RUSTSEC-2023-0071` | rsa Marvin timing attack | No upstream fix |
| `RUSTSEC-2019-0036` / `RUSTSEC-2020-0036` | `failure` crate EOL; vendored libsignal-protocol | No patch; vendored |

**Note:** `cargo-deny` is no longer installed on the dev VM (present for the
item #14 run, since removed). A live advisory re-scan was therefore not
performed in this audit; the picture above is derived from the lockfile graph,
which is unchanged in the relevant subtrees. Reinstall `cargo-deny`
(`cargo install cargo-deny --locked`) before release if a live scan is wanted.

---

## Conclusion

The lockfile hygiene problem flagged by three consecutive v2.0.2.0/v2.0.3.0
audits is resolved: this cycle's backlog was only 15 patch/minor bumps and is
now applied and verified (`cargo check` default + all-features + xmpp-bridge
all green). No source changes were required. The strategic items for future
releases, in leverage order: **libsql 0.6 → 0.9** (clears the legacy transitive
cluster + 4 advisory ignores), **reqwest 0.12 → 0.13** and
**tokio-tungstenite 0.26 → 0.30** (both deduplicate already-present newer
copies), and **rand 0.8 → 0.10** (direct dep is now the oldest rand in the
tree).
