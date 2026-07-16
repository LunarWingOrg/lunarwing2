# Architecture Documentation Refresh Design Spec

**Date:** 2026-07-15
**Status:** Implemented — approved and completed 2026-07-15
**Scope:** All eight files under `docs/architecture/`

## 1. Goal

Correct factual drift in the eight architecture documents so they match the current source code. The approach is targeted correction: fix what's wrong, add what's missing, leave what's already accurate untouched.

## 2. Non-Goals

- Full rewrites of any document
- Restructuring or reformatting documents that are already clear
- Changing any source code or test behavior
- Introducing new architecture documents
- Critical-only triage (the maintainer rejected that path; some non-critical corrections are in scope)

## 3. Evidence and Authority Rules

1. **Source code and tests outrank prose.** When a doc claims something about behavior, types, counts, versions, or file paths, the actual `.rs`/`.toml`/`.sh` file is authoritative. If they disagree, the doc changes.
2. **Preserve useful history.** If a doc records a design decision, rationale, or historical gap that's still relevant even though the code has moved on, keep the context but fix the factual claim.
3. **Preserve deferred status honestly.** If work was deferred and still is, the doc should say "deferred" with the original reasoning. If the work shipped, the doc should say so.
4. **No speculative corrections.** Every change must trace to a verified source-code finding from the audit. If a claim can't be confirmed against code, it stays as-is.
5. **Line numbers are hints, not contracts.** Several docs already state this. Corrections update the content, not pinned line numbers.

## 4. Editing Principles

- Minimal diff per file. Fix the wrong statement, don't rewrite the surrounding section.
- When a module, type, or file is missing from a map or table, add it in the right alphabetical or logical position.
- When a version number drifted, update every occurrence in that file.
- When a status label contradicts a later section in the same file, fix the stale label to match the authoritative section.
- Don't introduce TBD or TODO placeholders. Either the correction is known or it doesn't go in.

## 5. Per-File Corrections

### 5.1 ENGINE-V2.md

**Finding 1: EventKind variant count.**
The doc states "18 variants" (lines 31, 311). The actual `EventKind` enum in `ic/crates/lunarwing_engine/src/types/event.rs` has **20 variants**: StateChanged, StepStarted, StepCompleted, StepFailed, ActionExecuted, ActionFailed, LeaseGranted, LeaseRevoked, LeaseExpired, MessageAdded, ResponseDelta, ChildSpawned, ChildCompleted, ApprovalRequested, ApprovalReceived, SelfImprovementStarted, SelfImprovementComplete, SelfImprovementFailed, SkillActivated, OrchestratorRollback. Update the count in both locations (the module map comment on line 31 and the Event Sourcing section on line 311).

**Finding 2: Module map missing files.**
The module map (lines 22-64) omits three areas that exist in the source tree:

- `gate/` subdirectory: `pipeline.rs` (`GatePipeline`), `mod.rs` (`GateDecision`, `GateResolution`, `ExecutionMode`, `ResumeKind`), `tool_tier.rs` (tool-tier gate logic), `lease.rs` (lease-gated approval). The gate types are described in the Execution Gates section (lines 166-196) but the module map doesn't list the directory.
- `executor/llm_stream.rs`: LLM streaming support, added during the streaming phases. Present in the file tree but absent from the module map.
- `capability/planner.rs`: capability planning logic. Present in the file tree but absent from the module map.

Add these entries to the module map code block in their logical positions.

**Finding 3: Orchestrator module map entry.**
The doc describes `executor/orchestrator.rs` in a dedicated section (lines 125-132) but the module map (line 52 area) doesn't list it under `executor/`. Add it.

**Finding 4: Store trait method count.**
The doc claims "20 CRUD methods" in three locations (lines 39, 267, 300). The actual `Store` trait in `ic/crates/lunarwing_engine/src/traits/store.rs` defines **31 methods** (including required and default-implemented). Update all three occurrences from "20" to "31".

**Finding 5: ThreadType variants drifted.**
The doc (lines 78-81) lists three thread types: `Interactive`, `Background`, `SubThread`. The actual `ThreadType` enum in `ic/crates/lunarwing_engine/src/types/thread.rs` is: **`Foreground`**, **`Research`**, **`Mission`**. These names do not match the doc at all. Update the Thread Types subsection and any other reference to thread-type variant names in the document.

**Finding 6: ExecutionMode variants drifted.**
The doc (line 185) lists `Interactive | InteractiveWithAutoApprove | Unattended | Autonomous`. The actual `ExecutionMode` enum in `ic/crates/lunarwing_engine/src/gate/mod.rs` is: **`Interactive`**, **`InteractiveAutoApprove`**, **`Autonomous`**, **`Container`**. Note: `InteractiveWithAutoApprove` is actually `InteractiveAutoApprove`, `Unattended` does not exist, and `Container` is missing from the doc. Update the Execution Gates section.

### 5.2 SSH_DELIVERY_MECHANISMS.md

**Finding 1: russh version drift.**
The doc references "russh 0.45" in three places: line 89 ("the in-process **russh 0.45** client"), line 106 ("russh 0.45 signs `ssh-rsa` with SHA-1"), and line 296 (references the harness russh version). The actual dependency in `ic/Cargo.toml` line 148 is `russh = { version = "0.62", ... }`. Update all references from 0.45 to 0.62.

**Finding 2: RSA rejection wording.**
The RSA rejection note on line 106 prescribes a version-specific rationale ("russh 0.45 signs `ssh-rsa` with SHA-1, which modern servers reject"). Replace the entire rationale with the source-backed statement: LunarWing currently rejects RSA keys and accepts Ed25519/ECDSA only. Remove the SHA-1 explanation and any russh-version-specific reasoning. The corrected wording states the rejection as a current code-level fact with no external-library speculation.

**Finding 3: OpenCode worker omission.**
The worker-mode section (Mechanism 1, line 48-73) describes the agent creating a background job on an external worker (`nanocode/pebble`). The doc's "Quick decision" guidance and the at-a-glance table also list only nanocode and pebble as worker examples. OpenCode is now a supported worker type (the `create_job` path and `mt-admin` provisioning support opencode workers). Add OpenCode alongside nanocode and pebble in the Mechanism 1 description, the at-a-glance table, and any worker-enumeration references in this doc.

### 5.3 SSH_AGENT_HARNESS.md

**Finding 1: HostKeyVerifier status contradiction.**
Line 66 describes `HostKeyVerifier` as "**Built and unit-tested, but not yet wired into a live connection path**." This directly contradicts the same doc's section 7 (lines 319-330), which correctly states the verifier IS now wired live via `ssh_client.rs`. It also contradicts SSH_DELIVERY_MECHANISMS.md, which accurately describes live host-key verification.

Fix: update the component map entry (line 66) to match the authoritative section 7. The verifier is live for the in-process `ssh` tool and `ssh_git` tool paths.

**Finding 2: Component map missing `ssh_client.rs`.**
The component map (section 2, lines 60-68) lists five files: `ssh.rs`, `ssh_agent.rs`, `ssh_secrets.rs`, `ssh_hostkeys.rs`, `ssh_api.rs`, and `config/ssh.rs`. It omits `ic/src/bridge/ssh_client.rs`, the in-process russh client used by the `ssh` and WASM `ssh` tools. Section 7 references it by name but the component map doesn't list it. Add a row for `ssh_client.rs` describing its role: in-process russh client; `connect_and_exec`; wires `HostKeyVerifier` via russh's `Handler::check_server_key`.

**Finding 3: Module count.**
Section 2 lists six bridge modules (`ssh.rs`, `ssh_agent.rs`, `ssh_api.rs`, `ssh_client.rs`, `ssh_hostkeys.rs`, `ssh_secrets.rs`) plus the `config/ssh.rs` config-deserialization entry. With `ssh_client.rs` added to the table (Finding 2), the component table has **seven entries: six bridge modules plus the config module**. Ensure the doc's prose and table are consistent with this count.

**Finding 4: `host_key_verifier` field and accessor methods.**
The `SSHBridge` data-model section (line 109 area) lists fields but does not mention `host_key_verifier: Arc<HostKeyVerifier>` (verified at `ssh.rs:331`). The key-methods list (lines 116-119) omits two public methods: `load_key()` (`ssh.rs:577`) and `host_key_verifier()` (`ssh.rs:583`). Add the field to the struct field list and both methods to the key-methods list.

**Finding 5: russh version drift.**
Line 297 states "(`russh` / `russh-keys` 0.45, `ic/Cargo.toml:153`.)" The actual version is 0.62 (Cargo.toml line 148). Update the version and the line reference.

**Finding 6: Test coverage and integration statements are stale.**
Lines 360-362 state: "`HostKeyVerifier`, though well-tested in isolation, has no integration coverage because it is not wired in." This is no longer accurate — the verifier is wired live through `ssh_client.rs`. The correction: update the "Notable gaps" paragraph to state that the verifier is unit-tested and wired into the in-process client path. Update the test inventory with the verified counts: the six bridge `ssh*.rs` files contain **24 tests** (ssh.rs: 4, ssh_agent.rs: 1, ssh_secrets.rs: 6, ssh_hostkeys.rs: 9, ssh_api.rs: 1, ssh_client.rs: 3) and `config/ssh.rs` contains **2 tests**, totaling **26 tests** in the harness/config scope. Add `ssh_client.rs` with 3 tests to the per-file table. Replace the "no integration coverage because it is not wired in" statement with: "wired into the in-process client path; live-server integration test coverage remains an open gap."

### 5.4 SEMANTIC-MEMORY-SEARCH.md

**Finding 1: `openai_compatible` embedding provider is an explicit valid `EMBEDDING_PROVIDER` value.**
The doc's provider table (lines 86-91) lists OpenAI, Ollama, LunarWing Cloud, and Mock. The doc omits `openai_compatible` as a distinct, explicit provider value. The source (`ic/src/config/embeddings.rs` line 19, line 168) treats `openai_compatible` as a valid `EMBEDDING_PROVIDER` string value — it is not merely a functional implication of `EMBEDDING_BASE_URL` on the `openai` provider. When `EMBEDDING_PROVIDER=openai_compatible`, the config resolver selects `OpenAiEmbeddings` with `openai_base_url` and `OPENAI_API_KEY`. `EMBEDDING_BASE_URL` is optional and defaults to `https://api.openai.com`; it is required only to target a non-default compatible endpoint.

Correction: add `openai_compatible` as a row in the Supported Providers table, noting it targets any OpenAI-compatible endpoint via `OPENAI_API_KEY`, with `EMBEDDING_BASE_URL` optional (defaults to `https://api.openai.com`, required for non-default endpoints).

**Finding 2: File reference missing `workspace/hygiene.rs`.**
The File Reference table (lines 313-328) lists workspace modules but omits `ic/src/workspace/hygiene.rs`, which exists in the source tree. Add a row describing it as workspace cleanup/maintenance.

### 5.5 XMPP_FILE_TRANSFERS.md

**Finding 1: MIME allowlist description mismatch.**
Lines 121-126 describe supported attachment types as three buckets: "Image - `image/*`, Audio - `audio/*`, Document - everything else." The actual WASM host code (`ic/src/channels/wasm/host.rs`, lines 51-64) defines `ALLOWED_MIME_PREFIXES` as an explicit prefix list:

`image/`, `audio/`, `video/`, `application/pdf`, `application/vnd.`, `application/msword`, `application/rtf`, `text/`, `application/json`, `application/zip`, `application/gzip`, `application/x-tar`, `application/octet-stream`

The doc is wrong in two ways. First, `video/` is missing entirely from the doc's classification. Second, "Document - everything else" implies all non-image/audio types pass, but the host **rejects any MIME type that does not match one of the listed prefixes**.

Fix: replace the three-bucket description with the full current allowlist from `host.rs`. State explicitly that all other MIME types are rejected.

**Finding 2: AES-GCM IV length support.**
The doc's encrypted-media section (lines 77-78) mentions both 12-byte and 16-byte IV support for `aesgcm://` decryption. The source (`ic/src/channels/xmpp/mod.rs`, lines 3150-3156, 3163-3164) confirms both IV lengths are supported. This is already accurate in the doc. No correction needed for the IV claim — this finding is documented to confirm the audit verified it.

### 5.6 WEECHAT-CHANNEL-ARCHITECTURE.md

**Status: Reviewed — no factual corrections required.**

The audit raised two items. Both have been verified as accurate or factually correct in the source:

1. **Adapter restart claim.** The doc's statement that `restart-tenant <t>` restarts both the daemon and the adapter service is accurate. `restart_tenant()` calls `stop_tenant()` followed by `start_tenant()`; systemd stop explicitly includes the adapter service and start enables it (`Wants=`), and OpenRC explicitly starts/stops it. The existing claim holds.

2. **Endpoint labels.** The `GET /api/version` ("Adapter health probe"), `GET /api/config` ("Config refresh"), `GET /api/health`, and `GET /api/wait` endpoint descriptions in the ingestion section and per-cycle cost table are factually correct. No editorial change needed.

### 5.7 SELF_HEAL_DEPLOYMENT_WIRING.md

**Finding 1: Timer ownership clarity.**
The doc describes two installation paths and two "watchdogs," but the ownership boundary between them could be clearer. Specifically:

- `install-lunarwing-watchdog.sh` copies self-heal scripts to `/usr/local/sbin` but does NOT schedule them. The doc says this (lines 19-20, 88-92), which is correct.
- `add-tenant` via `ensure_health_pipeline()` DOES schedule the health pipeline. Also correct.
- What's unclear: on a host where `install-lunarwing-watchdog.sh` ran but `add-tenant` hasn't, the self-heal scripts are on disk but unscheduled. The doc mentions this at line 91-92 but the implication could be starker.

Fix: add a brief callout (one or two sentences) making explicit that the self-heal pipeline scheduling is owned exclusively by `add-tenant` / `ensure_health_pipeline()`, and that `install-lunarwing-watchdog.sh` will never schedule it. This prevents operators from assuming the watchdog installer handles both.

### 5.8 ATOMICBOOL_DEEPER_PROPAGATION.md

**Finding 1: Broken file reference for `ChatDelegate`.**
The doc (lines 26-27, 148-152) references `ChatDelegate` and lists `ic/src/llm/delegate.rs` as the file containing it. The file `ic/src/llm/delegate.rs` does not exist. `ChatDelegate` is defined in `ic/src/agent/dispatcher.rs` (line 261). The doc's "Files Likely Affected" section (line 152) lists the wrong path.

Fix: correct the file reference from `ic/src/llm/delegate.rs` to `ic/src/agent/dispatcher.rs` wherever it appears. Do not add new behavioral assertions about ghost output severity or empirical impact. The doc's existing "deferred by design" status and reasoning are accurate and should be preserved unchanged.

## 6. Verification Criteria

Each correction is complete when:

1. The corrected claim matches the source code or configuration file it references.
2. No internal contradiction remains within the same document (e.g., a component map that says "not wired" while a later section says "wired").
3. Cross-document consistency holds (e.g., SSH_DELIVERY_MECHANISMS and SSH_AGENT_HARNESS agree on HostKeyVerifier status and russh version).
4. The diff is minimal: only the wrong claim changed, no surrounding prose rewritten.
5. No new TBD, TODO, or placeholder text was introduced.

## 7. Completion Conditions

The refresh is done when:

- All eight files in `docs/architecture/` have been reviewed against the findings in section 5.
- Each finding is either applied or explicitly documented as "verified, no change needed" with a reason.
- No `docs/architecture/*.md` file contains a known factual contradiction with the source code.
- The `docs/README.md` architecture table descriptions still accurately summarize each file (spot-check only; full README audit is out of scope).
- No source code, test, or configuration file was modified during this work.
