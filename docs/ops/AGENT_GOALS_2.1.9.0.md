# PRE-RELEASE CHECKLIST for LunarWing v2.1.9.0

## Codename: UNKNOWN

**Open TODOs (v2.1.9.0) — To be done before release**

~~Old parity~~
~~Rares = odd (1, 3, 5, 7, ...)~~
~~AJ = even (2, 4, 6, 8, ...)~~
### mod-3 lanes (2026-10-04): Rares = 1 mod 3, AJ = 2 mod 3, Sweetiebot = 0 mod 3


#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [ ] Crate sweep for v2.1.9.0: bump every LunarWing-owned package from 2.1.8 to 2.1.9, update every external crate that can be updated, regenerate `ic/vendor/` and every lockfile, and keep CI/CD and the docs in step. Exit criteria: no LunarWing-owned package or path dependency left at 2.1.8; `cargo check` clean on all four feature combos; `cargo deny check` clean with no new advisory ignores; every advisory gate green; and a fully green MR pipeline (all jobs, worker lanes included) on the exact head being merged. Start from the current `aragents/kimil00p/v2.1.9.0` head, never a stale base, and read the spec below plus the v2.1.8.0 worked example before starting.
    - **Spec: LunarWing-owned packages (11).** Bump `version = "2.1.8"` to `"2.1.9"` in the daemon (`ic/Cargo.toml`), the five workspace crates (`ic/crates/lunarwing_{common,engine,health_metrics,safety,skills}`), the XMPP bridge (`ic/bridges/xmpp-bridge`) and the four WASM channels (`darkirc_channel_for_lunarwing/darkirc`, `ic/channels-src/multica`, `ic/channels-src/xmpp`, `lunarwing_weechat_wss/weechat_relay`; `ic/channels-src/{darkirc,weechat}` are symlinks), plus every inter-crate path-dependency requirement (`ic/Cargo.toml` ×4, `lunarwing_engine` ×3, `lunarwing_skills` ×1) and the owned-package entries of the affected lockfiles. `.gitlab/scripts/verify_release_version.py` checks the six `ic/` manifests against the release tag, so they must all agree; it accepts the release notes at the repository root or under `docs/releases/` since the v2.1.8.0 sweep, so no guard change is expected, but its `--self-test` must stay green. Writing the v2.1.9.0 release notes is a separate item, not part of the sweep.
    - **Spec: registry packages.** Patch-bump every channel registry entry (`ic/registry/channels/*.json`, currently darkirc 0.1.14, multica 0.1.17, weechat 0.2.15, xmpp 0.1.20) together with its crate. Bump a tool registry entry (`ic/registry/tools/*.json`) only when that tool's source or dependencies change in this sweep; a swept `ic/tools-src/*` lock root counts as a change (in v2.1.8.0 all nine tools moved). Keep each tool's locked surfaces identical (crate `Cargo.toml`, its `Cargo.lock` entry, `*.capabilities.json`, registry JSON; cavepony also pins `CAVEPONY_EXPECTED_VERSION` in `.gitlab/ci/integration.yml`) and prove it with `BASE_SHA=<target head> bash ic/scripts/check-version-bumps.sh` (the script reads the base from the environment, not from an argument). The !2100 review found the gitlab tool's pins out of step with its registry entry: check every surface of every tool you touch.
    - **Spec: external crates.** Run a semver-compatible `cargo update` (resolver v3 with the MSRV fallback; toolchain and `rust-version` stay 1.96.1) on the `ic` workspace and every tracked satellite lock root: 27 tracked `Cargo.lock` files outside `ic/vendor/`. Two of them are Pebble contracts, not sweep targets: `pebble4lunarwing/upstream/Cargo.lock` is manifest-less, gated by `pebble4lunarwing/scripts/verify_upstream_lock.py` and pinned by `PEBBLE_LOCK_SHA256` in `pebble4lunarwing/Dockerfile`, and `pebble/Cargo.lock` is the upstream snapshot behind `PEBBLE_REF`; both move only with a Pebble ref bump, which is a separate goal item. Semver-incompatible major bumps are out of scope: list each available one in the trail with the reason it was not taken (v2.1.8.0 left the RustCrypto family, rand, ed25519-dalek and tokio-tungstenite behind), and file a WI for any that matters. Do not raise the MSRV here.
    - **Spec: vendor tree.** Regenerate `ic/vendor/` only through `ic/scripts/refresh-vendor.sh`. Never hand-edit or delete vendored entries, keep the `libsignal-protocol{,-sys}` path dependencies, and let the script restore the WI#292 `rig-core` patch. The `[patch.crates-io]` override `ic/patches/wasmtime-internal-cache` is cut from wasmtime 36.0.20: if wasmtime moves, rebase the patch onto the new release in the same change (as WI#524 did for 36.0.20), keep its `LUNARWING-PATCH.md` current and keep the `zstd` acceptance it carries. Run `ic/scripts/vendor-artifact-guard.sh` on any reused target dir.
    - **Spec: advisories and policy.** `bash .gitlab/scripts/cargo-deny-check.sh` (advisories, bans, licenses, sources) and `check-satellite-advisories.sh`, `check-default-graph-advisories.sh` and `check-lockfile-only-advisories.sh` must all pass with zero new `ignore` entries in `ic/deny.toml` (13 acceptances today). Keep every existing version floor, for example the WI#524 wasmtime ban `>=36.0.0, <36.0.20`, and raise a floor only alongside its fix; make no other `ic/deny.toml` change. An advisory that lands mid-sweep is fixed by a bump, never an ignore, and recorded in the trail. A yanked pin is fixed by a bump in every lock that carries it plus the vendor refresh (WI#547: async-executor 1.14.0 → 1.14.1 needed `ic/Cargo.lock`, the XMPP bridge lock and `ic/vendor/`).
    - **Spec: worker and CI audit (record, do not upgrade).** Record the current pins with evidence: kimi-code (Dockerfile ARG equals the kimi-cli package pin), opencode v1, opencode2, the Pebble ref and its override lock digest (recompute it with `sha256sum`), the Debian snapshot (one `DEBIAN_SNAPSHOT` ARG per worker Dockerfile plus `CI_DEBIAN_SNAPSHOT` in `.gitlab/ci/defaults.yml`, tied together by the WI#561 validator), Node and Bun, the Rust toolchain, cargo-deny, shellcheck and sccache. External worker upgrades belong to a separate goal item. A CI tool bump (for example sccache) goes in its own commit so it can be reverted alone.
    - **Spec: tracked-manifest and lock audit.** `git grep '"2\.1\.8"' -- '*.toml' '*.lock' ':(exclude)ic/vendor'` must find no LunarWing-owned package or path dependency at 2.1.8. Historical prose (older release notes, audit docs, `ic/deny.toml` provenance comments, test fixtures that name a past tag) is exempt; list what remains and why.
    - **Spec: verification.** Follow the AGENTS.md build constraints (`taskset`, `-j`, no full debug builds, tmux for long runs, one cargo process at a time). Run `cargo check --locked` on default `--all-targets`, postgres-only, libsql-only and `--all-features`; `cargo clippy --locked --all --benches --tests --examples -- -D warnings` on default and `--all-features`; `cargo fmt --all -- --check`; `./scripts/build-wasm-extensions.sh --strict` for all tools and channels; `.gitlab/scripts/version-policy.sh`; `python3 .gitlab/scripts/verify_release_version.py --self-test`; `validate_config.py` and `registry_json.py`; and the `.gitlab/tests` suites that cover what you touched (`test_goal1_ci.py`, `test_deny_toml_guards.py`, `test_pebble_build_inputs.py` for any Pebble input). Then the MR pipeline, every job green, on the exact head. Check every lock-diff claim in the trail against the two locks rather than carrying it over from an earlier trail (the run-214 NX-19 and run-288 SY-2 lessons).
    - **Spec: commit hygiene.** One concern per commit: version bumps, external crate updates, vendor refresh, CI tool bumps, docs and trail. Stage no cargo-home gitlinks or other stray paths, and cherry-pick nothing from older sweep branches.
    - **Reference.** The v2.1.8.0 sweep is the worked example: branch `rarity/kimi-1-20261031-0000`, merged as `7ba88e9c1` via !2100, with its full trail under item 1 of the v2.1.8.0 goals file (`docs/ops/history/AGENT_GOALS_2.1.8.0.md`, or `git show v2.1.8.0:docs/ops/AGENT_GOALS_2.1.8.0.md`). Its run-288 review needed a follow-up for the gitlab tool's pins (SY-1) and a mis-stated `ic/Cargo.lock` delta (SY-2); the v2.1.7.0 predecessor !1906 was closed for building on a stale base, carrying an unneeded `ic/deny.toml` edit and mixing in the sccache bump

## Phase 2 — Features/Polish/BaselineAdditions (For goals referencing work items, read comments on work items as well. Also, see linked work items as well)

2. [ ] audit all crates for security and write report to docs/ops
3. [x] (S) Reserved

## Phase 3 — Carried Known-Issues from RELEASE-v2.1.8.0.md (For goals referencing work items, read comments on work items as well. Also, see linked work items as well)

4. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/545
    - **DONE 2026-11-07 (Rarity, item-4, branch `rarity/kimi-4-20261107-0002`):** fixed the original cache-poisoning defect on both completion paths. Empty, tool-free terminal responses still reach the caller but are never inserted, including reasoning-only and deterministic `length`/`content_filter` outcomes; regressions prove repeated empty blocking and streaming calls remain misses.
    - **Backend identity (comment finding D):** added `cache_key_model_name` with forwarding through retry, circuit-breaker, timeout, recording, stream-integrity and cache decorators. Smart routing and failover return no pre-dispatch identity, so their responses bypass the outer cache instead of being replayed from another model. This deliberately avoids the suggested post-dispatch-only rekey: it cannot make the earlier lookup safe and would consume failover's request-scoped model binding before the caller reads it. Stable single-backend caching is unchanged.
    - **Comment findings A–C:** cache keys now include sorted, length-framed request metadata and fail closed to a cache bypass on message serialization errors. Stream entries preserve the exact provider finish reason and retry/failover observations (including discarded usage), replaying them before terminal `Done`; an exact-chunk regression covers both. `docs/proposals/ENGINE_LLM_STREAMING.md` records the resulting contracts.
    - **Verification (`taskset -c 0-17`, `-j18`):** `cargo test --lib llm:: -- --nocapture` **663/663 PASS**, including all six new regressions; the post-Clippy metadata regression **1/1 PASS** on the exact final source. `cargo clippy --locked --all --benches --tests --examples -- -D warnings` PASS, `cargo fmt --all -- --check` PASS, and `git diff --check` PASS.
    - **Honest gap:** no live external-provider/tenant smoke or MR pipeline was run. The tests exercise the actual cache, routing/failover decorators and full composed chain with deterministic providers; item is checked on the code-fix bar.

## Phase 4 - Documentation and Misc goal items (For goals referencing work items, read comments on work items as well. Also, see linked work items as well)

5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/543

## Phase 5 - Misc Operations (ALWAYS READ ALL COMMENTS FOR EACH WORK ITEM WHEN A WORK ITEM IS REFERENCED - ALSO ALWAYS READ LINKED WORK ITEMS FOR EACH WORK ITEM REFERENCED)

6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/540

## Phase 6 - Gauntlets (for goals which reference any work items, see comments on those work items in addition to reading full descriptions)

7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/539
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/383
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/314
10. [ ] create draft for release notes at base of root

## Phase 7 - Advanced Operator Notes

11. [x] (S) Reserved

---
