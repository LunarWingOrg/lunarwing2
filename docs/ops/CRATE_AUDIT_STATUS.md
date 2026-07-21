# Crate Audit Status Report

**Date:** 2026-07-21
**Scope:** LunarWing v2.0.2.0 — `ic/` workspace and satellite crates
**Replaces:** `_OUTDATED_CRATE_AUDIT_STATUS.md` (2026-07-12, v2.0.0.0)

---

## Purpose

Verify the accuracy of the crate-audit info dump attached to
`AGENT_GOALS_2.0.2.0.md` item #17, document the current status of each flagged
crate, and identify any remaining issues.

---

## Summary Verdict

The info dump is **still accurate**. All six original claims remain verifiable
against the current lockfile. The three deferred crates (`rand`, `base64`,
`tower-http`) remain intentionally held back; their stated rationale is correct.

Changes since the 2026-07-12 audit:

- All LunarWing workspace crates advanced from `2.0.0` → `2.0.1`.
- The committed lockfile is **still behind** the latest compatible versions:
  a dry-run now reports **155 patch/minor updates**, **14 additions**, and
  **18 removals** (the previous audit counted ~116 updates).
- `cargo update` (patch/minor bumps) has **still not been run** against the
  committed lockfile. Recommendation unchanged: run it before release.
- `base64` is used directly across many call sites (not just
  `ssh_hostkeys.rs` as the old info dump stated — see correction below).

---

## Info Dump Claims — Verification

### 1. `tower-http` 0.6.10 → 0.7.0

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `tower-http = { version = "0.6", features = ["trace", "cors", "set-header", "catch-panic"] }` (`ic/Cargo.toml:91`) |
| **Locked version** | `0.6.10` (plus transitive `0.4.4` pulled by an older dep) |
| **Latest patch (in range)** | `0.6.11` — available via `cargo update` |
| **Latest minor/major** | `0.7.0` — requires Cargo.toml change |
| **Dry-run output** | `Updating tower-http v0.6.10 -> v0.6.11 (available: v0.7.0)` |
| **Status** | Deferred. Patch available but not yet applied. |
| **Info dump accuracy** | Correct — available but not required. |

### 2. `rand` 0.8.6 → 0.10.2

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `rand = "0.8"` (`ic/Cargo.toml:153`) — single direct declaration |
| **Locked versions** | `0.8.6`, `0.9.4`, `0.10.1` (transitive: `libsignal-protocol`, `wasmtime-wasi` via `cap-rand`, `rig-core` via `nanoid`) |
| **Available** | `0.8.7` (patch), `0.9.5`, `0.10.2` |
| **Breaking** | API redesign introduced in `0.9` |
| **Dry-run output** | `Adding rand v0.8.7 (available: v0.10.2)`, `Adding rand v0.9.5`, `Adding rand v0.10.2` |
| **Status** | Deferred. Two major versions behind, intentionally held. |
| **Info dump accuracy** | Correct. |

### 3. `base64` 0.21.7 → 0.22

| Field | Value |
|-------|-------|
| **Cargo.toml spec (main daemon)** | `base64 = "0.21"` (`ic/Cargo.toml:179`) |
| **Cargo.toml spec (satellites)** | `"0.22"` in `bridges/xmpp-bridge`, `channels-src/xmpp`, `tools-src/vision-analyze` |
| **Locked versions** | `0.21.7`, `0.22.1` |
| **Dry-run output** | (not listed — no patch update available within either minor range) |
| **Status** | Deferred. Dual-version lockfile entry is expected; main daemon migration not urgent. |
| **Info dump accuracy** | Mostly correct, **but the claim that `base64` is used "in `ssh_hostkeys.rs` for fingerprint computation" understates the usage.** Direct `use base64::Engine` / `base64::engine::general_purpose::*` call sites exist across at least: `src/setup/channels.rs`, `src/cli/oauth_defaults.rs`, `src/tools/builtin/ssh_git.rs`, `src/agent/attachments.rs`, `src/llm/transcription/chat_completions.rs`, `src/bridge/llm_adapter.rs`, `src/bridge/ssh_hostkeys.rs`, `src/channels/xmpp/omemo/store.rs`, `src/channels/http.rs`, `src/tools/mcp/auth.rs`, `src/tools/builtin/image_analyze.rs`, `src/channels/web/server.rs`. A 0.21→0.22 migration is mechanical (`Engine` trait path stable) but touches ~12 modules; still deferrable. |

### 4. `wasmparser` 0.220.1 — bundled with wasmtime 36

| Field | Value |
|-------|-------|
| **Cargo.toml spec** | `wasmparser = "0.220"` (`ic/Cargo.toml:140`) — for validation, independent of wasmtime |
| **Locked versions** | `0.220.1` (direct), plus `0.236.1`, `0.244.0`, `0.248.0` pulled transitively |
| **Dry-run output** | Removes the `0.244.0` and `0.248.0` duplicates; adds `0.254.0`; direct `0.220.1` stays |
| **Status** | Correct — tied to wasmtime 36. Independent update not needed. |
| **Info dump accuracy** | Correct — does not try to update independently. |

### 5. `pathdiff` being removed

| Field | Value |
|-------|-------|
| **Cargo.toml** | Not listed as a direct dependency |
| **Locked version** | `0.2.3` (transitive via the `open` crate) |
| **Dry-run output** | `Removing pathdiff v0.2.3` (newer `open` drops the dep) |
| **Status** | Will be removed by `cargo update`. Confirmed. |
| **Info dump accuracy** | Correct. |

### 6. `wasip3` being removed

| Field | Value |
|-------|-------|
| **Cargo.toml** | Not listed as a direct dependency |
| **Locked version** | `0.4.0+wasi-0.3.0-rc-2026-01-06` (transitive via `getrandom` → `uuid`) |
| **Dry-run output** | `Removing wasip3 v0.4.0+wasi-0.3.0-rc-2026-01-06` (newer `getrandom` drops it) |
| **Status** | Will be removed by `cargo update`. Confirmed. |
| **Info dump accuracy** | Correct. |

---

## `cargo update` Dry-Run Results

A `cargo update --dry-run` was executed on 2026-07-21 against `ic/Cargo.lock`.
Key counts: **155 updates**, **14 additions**, **18 removals** — all
semver-compatible (no Cargo.toml changes required).

Notable updates available:

- `tower-http` 0.6.10 → 0.6.11 (patch; `0.7.0` available with Cargo.toml change)
- `rand` 0.8.6 → 0.8.7 (patch within `"0.8"`); `0.10.2` available with breaking migration
- `chrono` 0.4.44 → 0.4.45
- `hyper` 1.9.0 → 1.11.0
- `http` 1.4.0 → 1.4.2
- `openssl` 0.10.79 → 0.10.81
- `rustls` 0.23.40 → 0.23.42
- `tokio` 1.52.3 → 1.53.1
- `wasm-bindgen` 0.2.121 → 0.2.126
- `zerocopy` 0.8.48 → 0.8.55
- `anyhow` 1.0.103 → 1.0.104
- `libc` 0.2.186 → 0.2.188
- `futures` 0.3.32 → 0.3.33
- `log` 0.4.29 → 0.4.33

Confirmed removals (transitive deadwood shed by newer deps): `pathdiff`,
`wasip3`, `wit-bindgen` 0.51.0 (+ `-core`/`-rust`/`-rust-macro`), `wit-component`
0.244.0, `wit-parser` 0.244.0, `wasm-encoder` 0.244.0/0.248.0, `utf8-width`,
`syn` 2.0.117 (replaced by 2.0.119 + a new 3.0.2).

Confirmed additions: `rand_pcg` 0.10.2, `minidom` 0.19.0, `sponge-cursor` 0.1.0,
plus version-bump additions for `rand`, `wasmparser`, `wasm-encoder`, `syn`,
etc.

**Recommendation:** Run `cargo update` followed by
`taskset -c 0-5 cargo check -j6 --all-features` and the targeted test suites
before the 2.0.2.0 release to pick up security patches and bug fixes.

---

## LunarWing Workspace Crate Versions (v2.0.2.0 Audit)

Item #4 of the prior 2.0.0.0 checklist required all LunarWing crates at `2.0.0`.
The workspace has since advanced to `2.0.1` on this branch. Verified status:

| Crate | Version | Location |
|-------|---------|----------|
| `lunarwing` (main daemon) | 2.0.1 | `ic/Cargo.toml` |
| `lunarwing_common` | 2.0.1 | `ic/crates/lunarwing_common/Cargo.toml` |
| `lunarwing_engine` | 2.0.1 | `ic/crates/lunarwing_engine/Cargo.toml` |
| `lunarwing_safety` | 2.0.1 | `ic/crates/lunarwing_safety/Cargo.toml` |
| `lunarwing_skills` | 2.0.1 | `ic/crates/lunarwing_skills/Cargo.toml` |
| `xmpp-bridge` | 2.0.1 | `ic/bridges/xmpp-bridge/Cargo.toml` |
| `xmpp-channel` (WASM) | 2.0.1 | `ic/channels-src/xmpp/Cargo.toml` |
| `weechat-relay-channel` (WASM) | 2.0.1 | `ic/channels-src/weechat/Cargo.toml` |
| `darkirc-channel` (WASM) | 2.0.1 | `ic/channels-src/darkirc/Cargo.toml` |
| `multica-channel` (WASM) | 2.0.1 | `ic/channels-src/multica/Cargo.toml` |

**WASM tools** use independent versioning (not tied to the daemon release):

| Tool | Version | Location |
|------|---------|----------|
| `gotify-tool` | 0.1.0 | `ic/lunarwing-gotify-tool/Cargo.toml`, `ic/tools-src/gotify/Cargo.toml` |
| `ssh-tool` | 0.1.0 | `ic/tools-src/ssh/Cargo.toml` |
| `vision-analyze-tool` | 0.2.0 | `ic/tools-src/vision-analyze/Cargo.toml` |
| `web-search-tool` | 0.2.0 | `ic/tools-src/web-search/Cargo.toml` |
| `llm-context-tool` | 0.1.0 | `ic/tools-src/llm-context/Cargo.toml` |
| `multica-bridge-tool` | 0.1.0 | `ic/tools-src/multica-bridge/Cargo.toml` |

These are WASM components with their own release cadence. This is intentional —
they are sandboxed plugins.

---

## Toolchain

- `rust-version = "1.92"` (`ic/Cargo.toml:15`, matches AGENTS.md MSRV).
- No `rust-toolchain.toml` pinned in `ic/`; the workspace builds against any
  toolchain ≥ 1.92.

---

## Outstanding Issues

### Deferred to post-2.0.2.0

1. **`rand` 0.8 → 0.10** — Breaking API redesign (0.9+). Direct dep in
   `ic/Cargo.toml` plus transitive use via `libsignal-protocol`, `wasmtime-wasi`
   (`cap-rand`), and `rig-core` (`nanoid`). Significant migration effort.
   **Defer.**

2. **`base64` 0.21 → 0.22** — Minor breaking (`Engine` trait path moves). The
   main daemon stays on `"0.21"` while `xmpp-bridge`, the xmpp WASM channel, and
   `vision-analyze-tool` already use `"0.22"`. A unified migration touches ~12
   modules but is mechanical. **Defer.**

3. **`tower-http` 0.6 → 0.7** — Minor version jump. Not urgent; the `0.6.11`
   patch (available via `cargo update`) is sufficient for now. **Defer.**

### Pre-2.0.2.0 Action

4. **`cargo update` not yet run** — The committed lockfile is behind the latest
   compatible versions by 155 patch/minor bumps. Run `cargo update` then
   `taskset -c 0-5 cargo check -j6 --all-features` and `cargo test --lib`
   before release to pick up security patches and bug fixes.

---

## Conclusion

The info dump attached to item #17 is **accurate**. All six original claims
remain verifiable:

1. `tower-http` 0.6 → 0.7: available, deferred (patch 0.6.11 still unapplied)
2. `rand` 0.8 → 0.10: available, deferred (breaking)
3. `base64` 0.21 → 0.22: available, deferred (minor breaking) — usage is broader
   than the old "ssh_hostkeys.rs only" note implied
4. `wasmparser` 0.220: bundled with wasmtime 36, fine
5. `pathdiff` removal: confirmed (transitive, drops with `cargo update`)
6. `wasip3` removal: confirmed (transitive, drops with `cargo update`)

No additional crates beyond those listed in the info dump were found to need
urgent attention. The workspace crate versions are all correctly at `2.0.1` on
this branch. The sole outstanding action before release is running
`cargo update` to apply the 155 accumulated patch/minor bumps.
