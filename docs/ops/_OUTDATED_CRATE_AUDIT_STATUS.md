# OUTDATED Crate Audit Status Report

**Date:** 2026-07-12
**Scope:** LunarWing v2.0.0.0 — `ic/` workspace and satellite crates

---

## Purpose

Verify the accuracy of the crate audit info dump in `AGENT_GOALS_2.0.0.0.md`
item #10, document the current status of each flagged crate, and identify
any remaining issues.

---

## Summary Verdict

The info dump is **accurate**. All claims are verifiable against the current
lockfile. The three deferred crates (rand, base64, tower-http) are
intentionally held back and are correctly documented as deferred to 2.0.0+.
A `cargo update` (patch/minor bumps) has **not yet been run** against the
committed lockfile — 116 packages are eligible for patch/minor updates.

---

## Info Dump Claims — Verification

### 1. `tower-http` 0.6.10 → 0.7.0

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `tower-http = { version = "0.6", features = [...] }` |
| **Locked version** | `0.6.10` |
| **Latest patch** | `0.6.11` (available via `cargo update`) |
| **Latest minor/major** | `0.7.0` (requires Cargo.toml change) |
| **Status** | Deferred. `0.6.11` patch available but not yet applied. |
| **Info dump accuracy** | Correct — available but not required. |

### 2. `rand` 0.8.6 → 0.10.2

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `rand = "0.8"` |
| **Locked version** | `0.8.6` |
| **Available** | `0.8.7` (patch, within `"0.8"` range) |
| **Breaking** | `0.9.x` and `0.10.2` exist (API redesign in 0.9+) |
| **Dependency tree** | Used directly by LunarWing, `libsignal-protocol` (vendored), `wasmtime-wasi` (via `cap-rand`), `rig-core` (via `nanoid`) |
| **Status** | Deferred. Two major versions behind, intentionally held. |
| **Info dump accuracy** | Correct. |

### 3. `base64` 0.21.7 → 0.22

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `base64 = "0.21"` (main crate) |
| **Locked version (0.21)** | `0.21.7` |
| **Locked version (0.22)** | `0.22.1` (used by xmpp-bridge, xmpp channel, vision-analyze tool) |
| **Breaking** | `0.22` has minor API changes (`Engine` trait moved) |
| **Dependency tree (0.21)** | Direct dep + transitive via `libsql`, `tonic` |
| **Status** | Deferred. Dual-version lockfile entry is expected. |
| **Info dump accuracy** | Correct — minor breaking, can be deferred. |

### 4. `wasmparser` 0.220.1 — bundled with wasmtime 36

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `wasmparser = "0.220"` |
| **Locked versions** | `0.220.1` (direct), `0.236.1` (wasmtime), `0.244.0`, `0.248.0` (other deps) |
| **Latest** | `0.253.0` (pulled by wasmtime update) |
| **Status** | Correct — tied to wasmtime 36. Independent update not needed. |
| **Info dump accuracy** | Correct — does not try to update independently. |

### 5. `pathdiff` being removed

| Field | Value |
|-------|-------|
| **Cargo.toml** | Not listed as a direct dependency |
| **Locked version** | `0.2.3` (transitive via `open` crate) |
| **Dry-run action** | `Removing pathdiff v0.2.3` (newer `open` drops the dep) |
| **Status** | Will be removed by `cargo update`. Confirmed. |
| **Info dump accuracy** | Correct. |

### 6. `wasip3` being removed

| Field | Value |
|-------|-------|
| **Cargo.toml** | Not listed as a direct dependency |
| **Locked version** | `0.4.0+wasi-0.3.0-rc-2026-01-06` (transitive via `getrandom` → `uuid`) |
| **Dry-run action** | `Removing wasip3` (newer `getrandom` drops it) |
| **Status** | Will be removed by `cargo update`. Confirmed. |
| **Info dump accuracy** | Correct. |

---

## `cargo update` Dry-Run Results

A `cargo update --dry-run` was executed on 2026-07-12. Key findings:

- **116 packages** eligible for patch/minor bumps within current Cargo.toml ranges
- **No breaking changes** introduced — all updates are semver-compatible
- Notable updates available:
  - `tower-http` 0.6.10 → 0.6.11 (patch)
  - `rand` 0.8.6 → 0.8.7 (patch within `"0.8"`)
  - `chrono` 0.4.44 → 0.4.45
  - `hyper` 1.9.0 → 1.10.1
  - `http` 1.4.0 → 1.4.2
  - `openssl` 0.10.79 → 0.10.81
  - `rustls` 0.23.40 → 0.23.41
  - `wasm-bindgen` 0.2.121 → 0.2.126
  - `zerocopy` 0.8.48 → 0.8.54
- Removals confirmed: `pathdiff`, `wasip3`, `wit-bindgen` 0.51.0, `wit-component` 0.244.0, `wit-parser` 0.244.0, `wasm-encoder` old versions, `utf8-width`

**Recommendation:** Run `cargo update` followed by `cargo check --all-features` to apply all safe patch/minor bumps before the 2.0.0.0 release.

---

## LunarWing Crate Versions (2.0.0 Audit)

Item #4 checked that all LunarWing crates are at `2.0.0`. Verified status:

| Crate | Version | Location |
|-------|---------|----------|
| `lunarwing` (main daemon) | 2.0.0 | `ic/Cargo.toml` |
| `lunarwing_common` | 2.0.0 | `ic/crates/lunarwing_common/Cargo.toml` |
| `lunarwing_engine` | 2.0.0 | `ic/crates/lunarwing_engine/Cargo.toml` |
| `lunarwing_safety` | 2.0.0 | `ic/crates/lunarwing_safety/Cargo.toml` |
| `lunarwing_skills` | 2.0.0 | `ic/crates/lunarwing_skills/Cargo.toml` |
| `xmpp-bridge` | 2.0.0 | `ic/bridges/xmpp-bridge/Cargo.toml` |
| `xmpp-channel` (WASM) | 2.0.0 | `ic/channels-src/xmpp/Cargo.toml` |
| `weechat-relay-channel` (WASM) | 2.0.0 | `ic/channels-src/weechat/Cargo.toml` |
| `darkirc-channel` (WASM) | 2.0.0 | `ic/channels-src/darkirc/Cargo.toml` |
| `multica-channel` (WASM) | 2.0.0 | `ic/channels-src/multica/Cargo.toml` |

**WASM tools** use independent versioning (not LunarWing release version):
- `gotify-tool` 0.1.0, `ssh-tool` 0.1.0, `vision-analyze-tool` 0.2.0, `web-search-tool` 0.2.0, `llm-context-tool` 0.1.0, `multica-bridge-tool` 0.1.0

These are WASM components with their own release cadence and are not tied to the main daemon version. This is intentional — they are sandboxed plugins.

---

## Outstanding Issues

### Deferred to post-2.0.0.0

1. **`rand` 0.8 → 0.10** — Breaking API redesign (0.9+). Touches direct usage in LunarWing + vendored `libsignal-protocol` + transitive deps (`wasmtime-wasi`, `rig-core`). Significant migration effort. **Defer.**

2. **`base64` 0.21 → 0.22** — Minor breaking (`Engine` trait moved). The main crate uses `"0.21"` while xmpp-bridge and some WASM sources already use `"0.22"`. A unified migration would clean up the dual-version lockfile entries but is low priority. **Defer.**

3. **`tower-http` 0.6 → 0.7** — Minor version jump. Not urgent; `0.6.11` patch is available and sufficient for now. **Defer.**

### Pre-2.0.0.0 Action

4. **`cargo update` not yet run** — The committed lockfile is behind the latest compatible versions by ~116 patch/minor bumps. Run `cargo update` then `cargo check --all-features` and `cargo test --lib` before release to pick up security patches and bug fixes.

---

## Conclusion

The info dump in item #10 is **fully accurate**. All six claims are verifiable:

1. tower-http 0.6 → 0.7: available, deferred
2. rand 0.8 → 0.10: available, deferred (breaking)
3. base64 0.21 → 0.22: available, deferred (minor breaking)
4. wasmparser 0.220: bundled with wasmtime 36, fine
5. pathdiff removal: confirmed (transitive, will drop with `cargo update`)
6. wasip3 removal: confirmed (transitive, will drop with `cargo update`)

No additional crates beyond those listed in the info dump were found to need
urgent attention. The workspace crate versions are all correctly at 2.0.0.

