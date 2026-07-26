# Crate Audit Status Report — v2.0.3.0

**Date:** 2026-07-25
**Scope:** LunarWing v2.0.3.0 — `ic/` workspace (`ic/Cargo.toml`) and satellite crates
**Supersedes:** `CRATE_AUDIT_STATUS.md` (2026-07-21, v2.0.2.0)

---

## Summary

- Workspace crate versions all correctly at **2.0.3** (item #1 of this checklist).
- `cargo update --dry-run` reports **157 patch/minor updates**, **14 additions**, **18 removals** — all semver-compatible, no Cargo.toml changes required.
- Previous audit (v2.0.2.0) reported 155 updates; the count has grown slightly and **`cargo update` has still not been run** against the committed lockfile.
- **3 new major-version upgrades are now available** that were not viable (or were riskier) at the time of the v2.0.2.0 audit — most notably **`tokio-xmpp` 5 → 6**, which resolves two RUSTSEC advisories currently silenced in `deny.toml`.
- The three previously-deferred migrations (`rand`, `base64`, `tower-http`) remain deferred with rationale unchanged.

**Top recommendation:** before shipping v2.0.3.0, run `cargo update` (picks up the 157 patch/minor bumps + transitive deadwood removal) and evaluate `tokio-xmpp` 5 → 6 (clears the two open hickory advisories). — **✅ DONE (item #10):** both applied; `cargo check`, clippy, and 62 XMPP unit tests pass.

---

## Workspace Crate Versions (v2.0.3.0)

All LunarWing workspace crates verified at `2.0.3`:

| Crate | Version | Location |
|-------|---------|----------|
| `lunarwing` (main daemon) | 2.0.3 | `ic/Cargo.toml` |
| `lunarwing_common` | 2.0.3 | `ic/crates/lunarwing_common/Cargo.toml` |
| `lunarwing_engine` | 2.0.3 | `ic/crates/lunarwing_engine/Cargo.toml` |
| `lunarwing_safety` | 2.0.3 | `ic/crates/lunarwing_safety/Cargo.toml` |
| `lunarwing_skills` | 2.0.3 | `ic/crates/lunarwing_skills/Cargo.toml` |
| `xmpp-bridge` | 2.0.3 | `ic/bridges/xmpp-bridge/Cargo.toml` |
| `xmpp-channel` (WASM) | 2.0.3 | `ic/channels-src/xmpp/Cargo.toml` |
| `weechat-relay-channel` (WASM) | 2.0.3 | `ic/channels-src/weechat/Cargo.toml` |
| `darkirc-channel` (WASM) | 2.0.3 | `ic/channels-src/darkirc/Cargo.toml` |
| `multica-channel` (WASM) | 2.0.3 | `ic/channels-src/multica/Cargo.toml` |

WASM tools use independent versioning (sandboxed plugins, not tied to the daemon release):

| Tool | Version | Location |
|------|---------|----------|
| `gotify-tool` | 0.1.0 | `ic/lunarwing-gotify-tool/Cargo.toml` |
| `ssh-tool` | 0.1.0 | `ic/tools-src/ssh/Cargo.toml` |
| `vision-analyze-tool` | 0.2.0 | `ic/tools-src/vision-analyze/Cargo.toml` |
| `web-search-tool` | 0.2.0 | `ic/tools-src/web-search/Cargo.toml` |
| `llm-context-tool` | 0.1.0 | `ic/tools-src/llm-context/Cargo.toml` |
| `multica-bridge-tool` | 0.1.0 | `ic/tools-src/multica-bridge/Cargo.toml` |

Toolchain: `rust-version = "1.92"` (`ic/Cargo.toml`), matches MSRV in AGENTS.md.

---

## Suggested Updates

### Tier 1 — High value, should evaluate before release ✅ DONE

#### 1. `tokio-xmpp` 5 → 6.0.0  ⭐ (security-relevant) ✅ DONE

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `tokio-xmpp = { version = "5", features = ["starttls"] }` (`ic/Cargo.toml:185`) |
| **Latest** | `6.0.0` |
| **Why it matters** | tokio-xmpp 5 pulls `hickory-resolver` 0.25, which carries **two RUSTSEC advisories currently silenced in `deny.toml`**: `RUSTSEC-2026-0118` (hickory-proto 0.25.x) and `RUSTSEC-2026-0119` (hickory-proto constrained by hickory-resolver 0.25 via tokio-xmpp 5.0.0). The `deny.toml` notes explicitly say these are "blocked on upstream tokio-xmpp release supporting hickory-resolver 0.26" — **that release now exists (6.0.0).** |
| **Risk** | Major version bump; XMPP channel + bridge are critical paths. Needs integration test pass. |
| **Recommendation** | **Evaluate for v2.0.3.0.** This is the single highest-value update: clears two open security advisories and removes the two most-legitimate `deny.toml` ignores. |

#### 2. `cargo update` — run it ✅ DONE

The committed lockfile is behind the latest compatible versions by **157 patch/minor bumps**. This is the third consecutive audit recommending this; it has still not been run.

Notable patch/minor updates included:
- `tower-http` 0.6.10 → 0.6.11
- `hyper` 1.9.0 → 1.11.0
- `tokio` 1.52.3 → 1.53.1
- `openssl` 0.10.79 → 0.10.81
- `rustls` 0.23.40 → 0.23.42
- `quinn` 0.11.9 → 0.11.11
- `http` 1.4.0 → 1.4.2
- `serde_json` 1.0.150 → 1.0.151
- `thiserror` 2.0.18 → 2.0.19
- `zerocopy` 0.8.48 → 0.8.55
- `wasm-bindgen` 0.2.121 → 0.2.126
- `regex` 1.12.3 → 1.13.1
- `time` 0.3.47 → 0.3.54
- `libc` 0.2.186 → 0.2.189
- `futures` 0.3.32 → 0.3.33
- `log` 0.4.29 → 0.4.33

Confirmed transitive deadwood shed by the update (removals): `pathdiff`, `wasip3`, `wit-bindgen` 0.51.0 (+ `-core`/`-rust`/`-rust-macro`), `wit-component` 0.244.0, `wit-parser` 0.244.0, `wasm-encoder` 0.244.0/0.248.0, `wasm-metadata` 0.244.0, `utf8-width`, `syn` 2.0.117 (replaced by 2.0.119 + a new 3.0.2 entry).

**Recommendation:** Run `cargo update`, then `taskset -c 0-5 cargo check -j6 --all-features`, then the targeted test suites before release. Low risk; picks up security patches and bug fixes.

### Tier 2 — Available major bumps

#### Done (v2.0.3.0)

| Crate | From | To | Cargo.toml | Notes |
|-------|------|----|------------|-------|
| `base64` | `0.21` | `0.22` | `ic/Cargo.toml:179` | Bumped to `0.22`, aligning with satellite crates (`xmpp-bridge`, `vision-analyze`, `ocr-sidecar`, `xmpp` channel) which were already on `0.22`. Code already used the `Engine` trait API, so zero source changes were required. Removes LunarWing's direct contribution to the duplicate `base64` lockfile entry. Note: `base64 0.21.7` remains in the lockfile transitively via upstream `libsql`/`libsql-hrana`/`tonic`/`tonic-reflection`; will clear when those upstreams publish `base64 ≥ 0.22` releases. Further bump to `0.23` is mechanical but deferred — no marginal benefit while transitive `0.21` persists. |

#### Deferred (track for future releases)

| Crate | Pinned | Latest | Cargo.toml | Notes |
|-------|--------|--------|------------|-------|
| `wasmtime` / `wasmtime-wasi` | `36.0.12` | `47.0.2` | `ic/Cargo.toml:138-139` | 11 major versions behind. WASM component-model is a core runtime path; large migration. **Defer** unless a WASM-platform requirement forces it. Track separately. |
| `reqwest` | `0.12` | `0.13.4` | `ic/Cargo.toml:39` | Major bump. Used widely (HTTP client for sandbox proxy, worker API, tunnel). **Defer.** |
| `tokio-tungstenite` | `0.26` | `0.30.0` | `ic/Cargo.toml:177,218` | 4 minor versions behind. WebSocket gateway path. **Defer** but track — the 0.26 line is starting to age. |
| `rand` | `0.8` | `0.10.2` | `ic/Cargo.toml:153` | Two majors behind. Breaking API redesign in 0.9+. Transitive use via `libsignal-protocol`, `wasmtime-wasi` (`cap-rand`), `rig-core` (`nanoid`). **Defer** — significant migration. |
| `tower-http` | `0.6` | `0.7.0` | `ic/Cargo.toml:91` | Minor version jump. Patch `0.6.11` (available via `cargo update`) is sufficient for now. **Defer.** |
| `rig-core` | `0.40` | `0.40.0` | `ic/Cargo.toml:157` | Already on latest 0.40 line. No action. |

### Tier 3 — No action needed

- `axum` 0.8 (latest line is 0.8.9 — on the current major).
- `tokio` 1, `serde` 1, `serde_json` 1, `anyhow` 1, `thiserror` 2, `tracing` 0.1, `clap` 4, `chrono` 0.4, `uuid` 1, `hyper` 1, `rustls` 0.23, `russh` 0.62 — all on the current major line; patch/minor bumps flow through `cargo update`.
- `wasmparser` 0.220 — bundled transitively with wasmtime 36; independent update not needed while wasmtime stays on 36.

---

## Advisory Status (`deny.toml`)

> **Update (2026-07-26, item #14):** `cargo-deny` 0.20.2 is now installed on the dev VM. A live `cargo deny check advisories` run confirmed that the two formerly ignored hickory advisories (`RUSTSEC-2026-0118`, `RUSTSEC-2026-0119`) are cleared by the item #10 `tokio-xmpp` 6 upgrade. Their ignores remain removed from `deny.toml`; the remaining five clusters are still legitimately blocked upstream and stay ignored.

A live advisory scan was run (`cargo deny check advisories`, cargo-deny 0.20.2) after the dependency upgrades and ignore cleanup. Advisory results:

| Advisory | Ignored reason | Status |
|----------|----------------|--------|
| `RUSTSEC-2026-0118` | hickory-proto 0.25.x; blocked on tokio-xmpp supporting hickory-resolver 0.26 | **Cleared** — tokio-xmpp 6.0.0 + hickory-resolver 0.26.1 applied (item #10); removed from `deny.toml`. |
| `RUSTSEC-2026-0119` | same chain as above | **Cleared** — same fix (item #10); removed from `deny.toml`. |
| `RUSTSEC-2026-0049` | rustls-webpki CRL distributionPoint; pinned by libsql transitive | Still blocked on upstream libsql. |
| `RUSTSEC-2026-0098` / `-0099` / `-0104` | rustls-webpki 0.102 via rustls 0.22 via libsql 0.6.0 | Still blocked on upstream libsql supporting rustls 0.23. |
| `RUSTSEC-2026-0187` | lopdf 0.34 via pdf-extract 0.7.12 | Still blocked on upstream pdf-extract. |
| `RUSTSEC-2023-0071` | rsa Marvin timing attack; no upstream constant-time release | Still open upstream. |
| `RUSTSEC-2019-0036` / `RUSTSEC-2020-0036` | `failure` crate EOL; vendored libsignal-protocol | No patch; vendored. |

**Actionable:** two of the seven distinct advisory clusters (`RUSTSEC-2026-0118`, `RUSTSEC-2026-0119`) are **cleared** via the `tokio-xmpp` 6 upgrade (item #10); both removed from `deny.toml`.

---

## Outstanding Actions

### Pre-v2.0.3.0 ✅ DONE (item #10, branch aj/item-10-20260726-0600)

1. **Run `cargo update`** ✅ — applied the 157 accumulated patch/minor bumps + transitive deadwood removal. `cargo check` and XMPP unit tests (62) pass.
2. **`tokio-xmpp` 5 → 6** ✅ — upgraded (with `xmpp-parsers` 0.22 → 0.23, required by the new tokio-xmpp re-export). Clears the two open hickory RUSTSEC advisories (`RUSTSEC-2026-0118`, `RUSTSEC-2026-0119`), both removed from `deny.toml`. `hickory-proto`/`hickory-resolver` now at 0.26.1. API migration in `src/channels/xmpp/mod.rs`: `disco::Feature` struct removed in 0.23 (features is now `BTreeSet<String>`); `Feature::new(ns)` → `ns.to_string()`, `feature.var` → direct iteration.

### Deferred (track for future releases)

3. `rand` 0.8 → 0.10 — breaking, broad transitive footprint.
4. ~~`base64` 0.21 → 0.22/0.23 — mechanical, ~12 modules.~~ **Done** — bumped to `0.22` (see Tier 2 done section). Further `0.22 → 0.23` deferred while transitive `0.21` persists via upstream `libsql`/`tonic`.
5. `tower-http` 0.6 → 0.7 — minor; patch sufficient for now.
6. `reqwest` 0.12 → 0.13 — major, wide usage.
7. `tokio-tungstenite` 0.26 → 0.30 — WebSocket gateway path.
8. `wasmtime` 36 → 47 — large; track against WASM-platform needs.

---

## Conclusion

Workspace versioning is correct (all 2.0.3). The lockfile is stale by 157 patch/minor bumps — `cargo update` should be run before release (this is the third audit in a row to say so). The genuinely new finding since the v2.0.2.0 audit is that **`tokio-xmpp` 6.0.0 has shipped**, which unblocks two RUSTSEC advisories currently silenced in `deny.toml`; that upgrade is the single most valuable dependency move available for this release.

**Update (item #10, branch aj/item-10-20260726-0600):** Both Tier 1 actions complete. `cargo update` applied (157 bumps + deadwood removal). `tokio-xmpp` 5 → 6.0.0 + `xmpp-parsers` 0.22 → 0.23 applied; the two hickory RUSTSEC advisories (`RUSTSEC-2026-0118`, `RUSTSEC-2026-0119`) are cleared and removed from `deny.toml`. `cargo check`, clippy (on touched file), and 62 XMPP unit tests pass.
