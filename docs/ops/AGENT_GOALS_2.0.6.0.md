# PRE-RELEASE CHECKLIST for LunarWing v2.0.6.0

## Codename: UNDECIDED

**Open TODOs (v2.0.6.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] Add support for parakeet-tdt-0.6b-v2-int8 and parakeet-tdt-0.6b-v3-int8 to lunarvoice - (original issue: implement proposal defined here - docs/proposals/lunarvoice-onnx-proposal-2.0.6.0.md — Completed: local sherpa-onnx sidecar, existing transcription-provider integration, sandboxed WASM tool, verified model workflow, multi-tenant systemd/OpenRC lifecycle, registry-aware health monitoring, tests, and deployment docs. additional work was done after this was written - ) — DONE 2026-08-08 (rarity/kimi-1-20260808-0601): `VOICE_STT_ENGINE=parakeet` added to voice-sidecar (sherpa-onnx transducer recognizer for parakeet-tdt-0.6b-v2-int8/v3-int8), engine-aware model path defaults + joiner artifact, mt-admin `voice_models_ready` engine-aware with VOICE_STT_ENGINE preserved across re-renders, manifest/env/README/proposal docs updated, sidecar unit tests + mt-admin shell tests (30/30 pass), cargo check/test/clippy/fmt clean.
2. [x] Fixed Sunburst BUG-003: conversation-insight payloads now exclude mission thread goals before applying the five-goal limit; mixed-thread regression coverage added. - work on this issue: Bug Report (Sunburst BUG-003) Summary Mission thread goal pollution — mission-spec text leaking into insight extraction. Severity High Source Consolidated from Sunburst's open bug list, last updated 2026-08-01 on build 2.0.3.0 (might already be fixed - in that case, confirm)
3. [x] MAKE V2 ENGINE CHANNELS ON BY DEFAULT FOR ALL i.e.  ENGINE_V2_CHANNELS=xmpp,weechat,darkirc by default should be present in new tenant env - ensure never overwrite existing tenant's value upon upgrade or rebuild or re-render — Completed 100%: `write_tenant_lunarwing_env` in ic/scripts/lunarwing-mt-admin.sh now writes ENGINE_V2=true + ENGINE_V2_CHANNELS=xmpp,weechat,darkirc by default for new tenants, and preserves existing tenant values across upgrade/rebuild/re-render via the standard `_env_existing` pattern (durable opt-out: ENGINE_V2=false or ENGINE_V2_CHANNELS=none). Fixture coverage added in ic/scripts/tests/test-add-tenant-env-flags.sh (all pass); docs updated (TENANT-CONFIGURATION.md, ENGINE-V2.md, README.md)
4. [ ] work on this issue: [BUG-013] Secret leak warnings in logs (noisy but no functional impact) - last updated 2026-08-01 on build 2.0.3.0 (might already be fixed - in that case, confirm) - PREVIOUS ATTEMPT on this branch HAD CI/CD ISSUES WITH SECRET DETECTION SCRIPT: aj/kimi-4-20260808-0601
5. [x] MCP part 2 work: host sandboxing (see docs for info) — Completed: opt-in per-server OS sandbox for host-local stdio MCP children (ic/src/tools/mcp/sandbox.rs). Portable unix: rlimits (memory/CPU/fds/processes/file-size, fail-closed above daemon hard limits), core-dump control, process-group isolation with group-wide teardown. Linux-only, fail-closed elsewhere: no_new_privileges, seccomp denial of ptrace/process_vm_readv/process_vm_writev, Landlock read-only root with explicit writable paths, and user+network namespace network isolation. Enforced in the fork/exec window on every spawn path (startup, activation, crash recovery share the factory). CLI: mcp add --sandbox + --sandbox-* flags; mcp list --verbose and doctor report the policy. Not covered (documented): device isolation, mandatory default profiles, inbound same-UID /proc/<pid>/mem reads (PR_SET_DUMPABLE does not survive execve; kernel credential checks are not syscall-filterable).
6. [ ] work on this issue: [BUG-013] Secret leak warnings in logs (noisy but no functional impact) - last updated 2026-08-01 on build 2.0.3.0 (might already be fixed - in that case, confirm) - PREVIOUS ATTEMPT HAD SECRET LEAK ISSUES IN CI/CD (on this branch): aj/kimi-6-20260808-1204
7. [ ] implement all remaining mcp part 2 work: general runtime adapters, MCP-over-WASM, and dedicated/shared gateway architecture (see docs for info)
8. [x] [BUG-016] WeeChat buffer restore — OpenRC root-vs-tenant context mismatch — Confirmed fixed in current source: OpenRC runs the graceful stop helper under the tenant UID via `start-stop-daemon --chuid`, allowing `/upgrade -quit` to reach the tenant-owned tmux socket and persist buffers; focused rendering, start-helper, and stop-helper regressions pass.
9. [ ] Research the following and make a new doc in ops with findings and next steps for this ( referencing docs/ops/35d44998cadf3e6d.md ): LunarHub / IronHub / Extension Marketplace — Future Direction - What they did: IronHub deep link installs, custom MCP server registration from WebUI with OpenAPI-derived tool discovery bounded by manifest tool budget. Extension lifecycle with CAS-protected mutations, hash-pinned manifests, soft-removal tombstones, restart repair. - What LunarWing needs first: 1 Binary distribution (the identified blocker) — proper packaged releases for LunarWing binaries, 2 Manifest registry — could be hosted on git.lunarwing.org, 3 Install flow with provenance verification — MCP provenance pinning already landing in 2.0.6.0 - Status: Pieces are being built. Marketplace is post-2.0.7.0 at earliest
10. [ ] Research wasm functionality expansion n make new doc in doc/proposals
11. [ ] mcp part 2 work worker-local placement (see docs for info)
12. [ ] fix cicd issues
13. [ ] Review and make fixes to MCP part two implementation (see mcp documents first) - address any gaps or problems
14. [ ] FIX CI/CD ISSUES
15. [ ] review previous mcp work for part two and fix anything if something is found out to be weird or broken
16. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.5.0
17. [ ] go over items in work items ( hint: can use gitlab api to access work items without auth ). make note/document of any that are already confirmed fix and should be closed - put note/doc in docs/ops - check when done
18. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.6.0 explaining all relevant changes since v2.0.5.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.6.0.md` and should be written to the ROOT of the repo
19. [ ] update any outdated documents in docs/proposals
20. [ ] update any outdated documents in docs/guides
21. [ ] update README.md in repo root (if necessary)
22. [ ] review first draft of release notes from prev step and update any inconsistencies
23. [ ] update readme from prev step and update any inconsistencies
24. [ ] Run cargo update and lockfile reconciliation before tag
25. [ ] check cicd of master for failures

---
