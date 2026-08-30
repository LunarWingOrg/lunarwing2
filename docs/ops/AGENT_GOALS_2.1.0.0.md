# PRE-RELEASE CHECKLIST for LunarWing v2.1.0.0

## Codename: UNKNOWN

**Open TODOs (v2.1.0.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine & architecture)

1. [x] Update all external crates that need updating for the entire project. See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: `cargo check` clean on all feature combos, `cargo deny check` clean, CI green on a scratch branch.
    - DONE 100% 2026-08-30 (rarity/kimi-1-20260830-1801): full lockfile refresh + targeted major bumps + CI/worker-pin sync, all exit criteria met except the pipeline cannot be observed from this box (no git push performed, per host rules — pipeline runs on push/MR of this scratch branch).
      - (lockfiles) `cargo update` across all 19 tracked lockfiles (main ic/ + darkirc, gotify-tool ×3, xmpp bridge+channel, multica, test-echo fixture, unix-socket client + replv2git copy, vendored libsignal, weechat_relay, pebble, pebble4lunarwing, ocr/voice sidecars). Notables: h2 0.4.16→0.4.19, hyper 1.11.0→1.11.1, uuid 1.24→1.26, wasmtime tree 36.0.13→36.0.14, rustls-webpki 0.103.14→0.103.15, wat/wast 256→258, syn 3.0.3→3.0.4.
      - (fs4 removed entirely) fs4 dropped — `std::fs::File` locking stabilized in Rust 1.89 covers all six call sites (bootstrap PidLock, gate persistence, pairing store, DarkIRC key-manager busy loop, MCP config write guard). WouldBlock now matched via `std::fs::TryLockError::WouldBlock`. MSRV 1.96.1 confirms API availability. One dependency deleted.
      - (html-to-markdown-rs 2.3→3): `convert()` now returns `ConversionResult { content: Option<String>, .. }`; adapted without unwrap (existing ToolError path). 3/3 html_converter tests.
      - (ocr-sidecar) reqwest = "0.12" → "0.13" + base64 0.22→0.23 — repo reqwest graph back to a single 0.13 line (2.0.7 consolidation precedent). 4/4 sidecar tests.
      - (CI/worker pins) KIMI_CODE_VERSION 0.39.0→0.39.1, OPENCODE_REF v1.18.23→v1.18.25; synced Dockerfile ARG defaults, `.gitlab/ci/containers.yml`, `docs/ops/WORKER-CONTAINERS.md`, `docs/ops/GITLAB_CONTAINER_REGISTRY.md`, `kimicode4lunarwing/AGENTS.md`.
      - (security scan) Fresh RustSec scan of all 19 lockfiles with the advisory ignore list zeroed: only the previously accepted findings remain (vendored `failure` RUSTSEC-2019-0036/2020-0036; rsa RUSTSEC-2023-0071 via russh; libsql-0.6-chain webpki RUSTSEC-2026-0049/-0098/-0099/-0104; h2 0.3.27 RUSTSEC-2026-0258 on the same libsql chain; the same unmaintained set as before). No new advisories; pebble's bincode/yaml-rust unmaintained findings pre-exist unchanged and pebble is a separate CI lane.
      - (intentionally deferred majors, unchanged from the 2.0.8.0 reality) libsql 0.6→0.9 (2.0.8.0 audit: even 0.9.30 keeps the rustls-0.22/webpki-0.102 chain — separate security-reviewed work), zbus 4→5/secret-service 5 (keyring rework), RustCrypto suite sha2/aes-gcm/hmac/hkdf plus ed25519-dalek 2→3 and russh (2.0.8.0 audit: pinned `rsa` has no upstream constant-time release yet), rand 0.8→0.9 (API churn), wasmtime 36→48 (WIT/component churn — own cycle), zip 2→8, tokio-tungstenite 0.26→0.30, bollard 0.18→0.21, agent-client-protocol 0.10→2.0, rig-core 0.40→0.42. Same defer-with-reason stance as the 2.0.8.0 CRATE_SECURITY_AUDIT.
      - Verify: `cargo check` clean on all four feature combos (default / postgres-only / libsql-only / all-features); `cargo deny check` (advisories+bans+licenses+sources) clean; wasm32-wasip2 `cargo check` green for darkirc, multica, xmpp channel, gotify tool; `cargo clippy --lib -- -D warnings` clean; `cargo fmt --all -- --check` clean; lock-migration tests: pid_lock 8/8, pairing::store 14/14, darkirc_key_manager 89/89, tools::mcp::config 67/67, html_converter 3/3; ocr-sidecar 4/4 under reqwest 0.13. CI green on this scratch branch cannot be observed from this box without pushing — pipeline runs on push/MR; all in-repo CI inputs (deny config, CI scripts, worker pins) are consistent with the new graph.
2. [ ] Update all LunarWing crates from 2.0.9 to 2.1.0. Other crates such as WASM channel/tool crates that don't specifically carry these versions must also be updated in order to pass CI/CD requirements. Exit criteria: no stale version strings repo-wide (`rg '2\.0\.9'` returns only intentional historical references), CI green.
3. [ ] Mid-turn interrupt + steer (Hermes-style). Interrupt must abort an in-flight LLM stream and tool execution mid-turn — not only at turn boundaries (current state: `TurnState::Interrupted` flag in `session.rs`, no cancel token in the agentic loop, no stream abort). Steer injects a mid-turn note that arrives after the current tool call completes, without breaking role alternation or the tool-calling loop (queue-front re-queue plumbing exists at `session.rs:326` as a starting point). Exit criteria: regression tests for interrupt-during-stream and steer-during-tool-loop; live verification on a test tenant; no regression to existing turn-boundary interrupt behavior.
    - (context) Analysis from 2026-08-20: interrupt hardening moderate (AtomicBool between steps + tokio::select!), steer moderate-large (steer slot drained after tool results). Unique differentiator vs OpenClaw/Claude Code. Lineage: Kumogakure's COOL_THINGS_THAT_HERMES_AGENT_HAS.md. Was approved for 2.0.9 and deferred — now due.
4. [ ] LunarVoice completion (currently ~30-40% from the 2.0.7 era). Finish the service to production quality: full lifecycle (install/configure/activate/remove), health integration, and documented config surface. Exit criteria: feature-complete on a test tenant, health checks green, docs updated in the same branch.
    - (context) LunarVoice health hardening landed in v2.0.7.0 sidecar rework; desktop/Android integration (Phase 3 items) depends on this being solid first.
5. [ ] Per-healthpoint metrics exporters — evolve health services beyond "send notification on recovery failure" to configurable early-warning signals: a dedicated metrics exporter for every healthpoint, exposed per-tenant through the ports registry (follow the health_metrics v16 slot pattern). Exit criteria: every existing healthpoint emits metrics, thresholds configurable, docs updated
6. [ ] F4 hardening — port the passive-voice false-claim detector extension. The kept stash (`stash@{0}` on the 2.0.9 dev branch, `default.py`) adds passive-voice completion claims ("was written", "was saved", "was stored", "was persisted", "was retrieved") and space-separated tool mentions ("memory write", "memory/") to `signals_claimed_tool_success()`. Re-port against current release code (do NOT pop the stale stash blindly — its base predates the shipped F4/F5 rework). Exit criteria: patterns merged into the shipped detector, regression tests for passive-voice claims, no false-positive regressions on the existing test suite.
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/95
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/94
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/96
10. [ ] use your browser tool to work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/93

## Phase 2 — Release hygiene

11. [ ] Document the orchestrator's 2-retry policy as intentional (WI#88 follow-up). One-paragraph note in the relevant docs stating the hardcoded 2-retry `double-timeout` policy is by design — a knob was considered and rejected — so future readers don't re-litigate it. Closes Kageho's review question.
    - (context) Kageho's review of the #88 fix: "I can give this a 4+ if you unblock the double-timeout knob (or document it as deferred)." Christopher decided: intentional, not deferred.
12. [ ] Come up with codename candidates for this release cycle. Put into a new doc under docs/ops following the Japanese word/name convention (no repeats of past codenames), themed on this cycle's actual work — interrupt/steer responsiveness, voice, mascot/personality, surfaces. The v2.0.5.0 CODENAME_IDEAS.md is the format reference.
    - (context) 2.0.9.0 shipped as Inazuma (稲妻, lightning); 2.0.8.0 was Sekisho. Prior picks doc: v2.0.5.0/CODENAME_IDEAS.md (Kagi/Chiyu/Hagane — do not reuse).
13. [ ] First-draft release notes (at root of repo) as RELEASE-v2.1.0.0.md, covering all changes since v2.0.9.0, with an accurate known-issues list. Use docs/releases history for format reference. A second item later in the cycle improves accuracy of this draft.
14. [ ] Update README.md in repo root (if necessary) — reflects 2.1-era reality: new surfaces pointers, no stale 2.0.9-specific detail.

## Phase 3 — Surfaces & delight (the 2.1 identity)

15. [ ] Emoting mascot in the web gateway. An animated avatar widget that mirrors the agent's expressed emotions: parse the agent's outgoing roleplay action phrases ("*stamps hooves*", "*ears perk*") on the gateway's outgoing-message path and map them to sprite animation frame sets. Build gateway-first — the desktop app inherits it. Exit criteria: emote-parser hook live in `src/channels/web/`, phrase→enum→sprite state machine working, a first sprite pack (pony/unicorn) rendering in the GUI as a draggable corner widget, idle/working fallback animations. Details: study-note topic emoting-mascot-2-1 / fact 206 - also see: desktop app
    - (context) Full implementation sketch in the study-notes knowledge base, topic `emoting-mascot-2-1` (also fact 206). Origin: user suggestion. Christopher has sprite-animation experience and plans to make the art. petdex (192×208 frame convention) is prior art. Presentation layer only — zero core-path risk.
16. [ ] Desktop app — POINTER ITEM. LunarWing desktop application (own repo, own goals file, to be created): native app wrapping the web gateway experience with the emoting mascot widget and LunarVoice integration as headline features. THIS item tracks only: repo created, goals file seeded (mascot + voice + gateway-integration items), build scaffold green. Implementation happens in that repo's own goals cycle, not here.
    - (context) Hermes desktop app (Electron) is feature reference: HUD mode, worktrees, Memory Graph, remote backends. Create the repo when Phase 1 voice work is far enough along to integrate. (repo created in gitlab; no goals file yet; may append more details later)
17. [ ] Android app — POINTER ITEM. Same treatment as item 12: own repo, own goals file, mascot widget + voice + gateway features. THIS item tracks only repo creation + goals seeding + buildable scaffold (repo created in gitlab now; no goals file yet; may append more details later) 
18. [ ] Release-notes accuracy pass on RELEASE-v2.1.0.0.md (the item 9 draft) — verify every claim against the actual shipped changes; known-issues list reconciled against the work-item tracker.

## Deferred (explicitly NOT this release)

- **LTX** — next major target after LunarVision + LunarVoice are solid; 2.2-era.
- **DarkIRC Stages B-D** — remain disabled and separately gated behind Stage A acceptance (see item 3 in the 2.0.9.0 list / DARKIRC-KEYEXCHANGE-STATUS.md).

---

*(Rebuilt 2026-08-30 from the v2.1 planning session: foundation → hygiene → surfaces. Sub-bullet DONE-trail convention adopted file-wide per the 2.0.9 lessons — inline evidence walls caused agent confusion and one planning-death.)*
