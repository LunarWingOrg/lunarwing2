# PRE-RELEASE CHECKLIST for LunarWing v2.1.0.0

## Codename: UNKNOWN

**Open TODOs (v2.1.0.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine & architecture)

1. [ ] Update all external crates that need updating for the entire project. See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: `cargo check` clean on all feature combos, `cargo deny check` clean, CI green on a scratch branch.
2. [ ] Update all LunarWing crates from 2.0.9 to 2.1.0. Other crates such as WASM channel/tool crates that don't specifically carry these versions must also be updated in order to pass CI/CD requirements. Exit criteria: no stale version strings repo-wide (`rg '2\.0\.9'` returns only intentional historical references), CI green.
3. [ ] Mid-turn interrupt + steer (Hermes-style). Interrupt must abort an in-flight LLM stream and tool execution mid-turn — not only at turn boundaries (current state: `TurnState::Interrupted` flag in `session.rs`, no cancel token in the agentic loop, no stream abort). Steer injects a mid-turn note that arrives after the current tool call completes, without breaking role alternation or the tool-calling loop (queue-front re-queue plumbing exists at `session.rs:326` as a starting point). Exit criteria: regression tests for interrupt-during-stream and steer-during-tool-loop; live verification on a test tenant; no regression to existing turn-boundary interrupt behavior.
    - (context) Analysis from 2026-08-20: interrupt hardening moderate (AtomicBool between steps + tokio::select!), steer moderate-large (steer slot drained after tool results). Unique differentiator vs OpenClaw/Claude Code. Lineage: Kumogakure's COOL_THINGS_THAT_HERMES_AGENT_HAS.md. Was approved for 2.0.9 and deferred — now due.
4. [ ] LunarVoice completion (currently ~30-40% from the 2.0.7 era). Finish the service to production quality: full lifecycle (install/configure/activate/remove), health integration, and documented config surface. Exit criteria: feature-complete on a test tenant, health checks green, docs updated in the same branch.
    - (context) LunarVoice health hardening landed in v2.0.7.0 sidecar rework; desktop/Android integration (Phase 3 items) depends on this being solid first.
5. [ ] Per-healthpoint metrics exporters — evolve health services beyond "send notification on recovery failure" to configurable early-warning signals: a dedicated metrics exporter for every healthpoint, exposed per-tenant through the ports registry (follow the health_metrics v16 slot pattern). Exit criteria: every existing healthpoint emits metrics, thresholds configurable, docs updated.
6. [ ] F4 hardening — port the passive-voice false-claim detector extension. The kept stash (`stash@{0}` on the 2.0.9 dev branch, `default.py`) adds passive-voice completion claims ("was written", "was saved", "was stored", "was persisted", "was retrieved") and space-separated tool mentions ("memory write", "memory/") to `signals_claimed_tool_success()`. Re-port against current release code (do NOT pop the stale stash blindly — its base predates the shipped F4/F5 rework). Exit criteria: patterns merged into the shipped detector, regression tests for passive-voice claims, no false-positive regressions on the existing test suite.

## Phase 2 — Release hygiene

7. [ ] Document the orchestrator's 2-retry policy as intentional (WI#88 follow-up). One-paragraph note in the relevant docs stating the hardcoded 2-retry `double-timeout` policy is by design — a knob was considered and rejected — so future readers don't re-litigate it. Closes Kageho's review question.
    - (context) Kageho's review of the #88 fix: "I can give this a 4+ if you unblock the double-timeout knob (or document it as deferred)." Christopher decided: intentional, not deferred.
8. [ ] Come up with codename candidates for this release cycle. Put into a new doc under docs/ops following the Japanese word/name convention (no repeats of past codenames), themed on this cycle's actual work — interrupt/steer responsiveness, voice, mascot/personality, surfaces. The v2.0.5.0 CODENAME_IDEAS.md is the format reference.
    - (context) 2.0.9.0 shipped as Inazuma (稲妻, lightning); 2.0.8.0 was Sekisho. Prior picks doc: v2.0.5.0/CODENAME_IDEAS.md (Kagi/Chiyu/Hagane — do not reuse).
9. [ ] First-draft release notes (at root of repo) as RELEASE-v2.1.0.0.md, covering all changes since v2.0.9.0, with an accurate known-issues list. Use docs/releases history for format reference. A second item later in the cycle improves accuracy of this draft.
10. [ ] Update README.md in repo root (if necessary) — reflects 2.1-era reality: new surfaces pointers, no stale 2.0.9-specific detail.

## Phase 3 — Surfaces & delight (the 2.1 identity)

11. [ ] Emoting mascot in the web gateway. An animated avatar widget that mirrors the agent's expressed emotions: parse the agent's outgoing roleplay action phrases ("*stamps hooves*", "*ears perk*") on the gateway's outgoing-message path and map them to sprite animation frame sets. Build gateway-first — the desktop app inherits it. Exit criteria: emote-parser hook live in `src/channels/web/`, phrase→enum→sprite state machine working, a first sprite pack (pony/unicorn) rendering in the GUI as a draggable corner widget, idle/working fallback animations.
    - (context) Full implementation sketch in the study-notes knowledge base, topic `emoting-mascot-2-1` (also fact 206). Origin: user suggestion. Christopher has sprite-animation experience and plans to make the art. petdex (192×208 frame convention) is prior art. Presentation layer only — zero core-path risk.
12. [ ] Desktop app — POINTER ITEM. LunarWing desktop application (own repo, own goals file, to be created): native app wrapping the web gateway experience with the emoting mascot widget and LunarVoice integration as headline features. THIS item tracks only: repo created, goals file seeded (mascot + voice + gateway-integration items), build scaffold green. Implementation happens in that repo's own goals cycle, not here.
    - (context) Hermes desktop app (Electron) is feature reference: HUD mode, worktrees, Memory Graph, remote backends. Create the repo when Phase 1 voice work is far enough along to integrate. (repo created in gitlab; no goals file yet)
13. [ ] Android app — POINTER ITEM. Same treatment as item 12: own repo, own goals file, mascot widget + voice + gateway features. THIS item tracks only repo creation + goals seeding + buildable scaffold (repo created in gitlab now; no goals file yet) 
14. [ ] Release-notes accuracy pass on RELEASE-v2.1.0.0.md (the item 9 draft) — verify every claim against the actual shipped changes; known-issues list reconciled against the work-item tracker.

## Deferred (explicitly NOT this release)

- **LTX** — next major target after LunarVision + LunarVoice are solid; 2.2-era.
- **DarkIRC Stages B-D** — remain disabled and separately gated behind Stage A acceptance (see item 3 in the 2.0.9.0 list / DARKIRC-KEYEXCHANGE-STATUS.md).

---

*(Rebuilt 2026-08-30 from the v2.1 planning session: foundation → hygiene → surfaces. Sub-bullet DONE-trail convention adopted file-wide per the 2.0.9 lessons — inline evidence walls caused agent confusion and one planning-death.)*
