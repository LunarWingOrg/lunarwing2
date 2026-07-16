# Architecture Documentation Refresh Implementation Plan

**Status:** Implemented 2026-07-15

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correct factual drift in all eight `docs/architecture/*.md` files so they match the current source code, with no source-code changes.

**Architecture:** Targeted corrections only. Each task edits one architecture doc (or one tightly coupled pair) by replacing specific stale phrases with verified current facts. No document rewrites, no structural changes, no new documents.

**Tech Stack:** Markdown documentation files under `docs/architecture/`. No build system, no tests, no dependencies.

## Global Constraints

Copied verbatim from the approved spec (`docs/superpowers/specs/2026-07-15-architecture-doc-refresh-design.md`):

- **No source code changes.** This work touches `docs/architecture/*.md` files only. Do not modify any `.rs`, `.toml`, `.sh`, `.sql`, or configuration file. Do not edit files under `ic/`.
- **Source code outranks prose.** Every correction must trace to a verified source-code finding. The `.rs`/`.toml`/`.sh` file is authoritative.
- **Minimal diff per file.** Fix the wrong statement; don't rewrite the surrounding section.
- **No new TBD, TODO, or placeholder text.** Either the correction is known or it doesn't go in.
- **No speculative corrections.** If a claim can't be confirmed against code, it stays as-is.
- **Preserve deferred status honestly.** If work was deferred and still is, keep the original reasoning.
- **No regression tests required.** These are docs-only edits. Verification is by Grep/Read of the target files, not by cargo or test runs.
- **No commit steps.** The user did not request a commit. Do not run git commit or git add.

---

## File Structure

| Task | File(s) | Corrections |
|------|---------|-------------|
| Task 1 | `docs/architecture/ENGINE-V2.md` | 6 findings: variant counts, module map, thread types, execution modes |
| Task 2 | `docs/architecture/SSH_DELIVERY_MECHANISMS.md` | 3 findings + crate-path fix: russh version, RSA rationale, OpenCode worker, `russh_keys::`→`russh::keys::` |
| Task 3 | `docs/architecture/SSH_AGENT_HARNESS.md` | 6 findings + crate-path fix: verifier status, component map, data model, version, test counts, `russh_keys::`→`russh::keys::` |
| Task 4 | `docs/architecture/SEMANTIC-MEMORY-SEARCH.md` | 2 findings: provider table, file reference table |
| Task 5 | `docs/architecture/XMPP_FILE_TRANSFERS.md` | 1 finding: MIME allowlist description |
| Task 6 | `docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md` | 1 finding: timer ownership callout |
| Task 7 | `docs/architecture/ATOMICBOOL_DEEPER_PROPAGATION.md` | 1 finding: broken file reference |
| Task 8 | `docs/architecture/WEECHAT-CHANNEL-ARCHITECTURE.md` | No changes (verified accurate) |
| Task 9 | Cross-doc verification pass | No edits; Grep/Read verification |

---

### Task 1: ENGINE-V2.md — Variant counts, module map, thread types, execution modes

**Files:**
- Modify: `docs/architecture/ENGINE-V2.md`

**Interfaces:**
- Consumes: none (first task, no dependencies)
- Produces: corrected ENGINE-V2.md for cross-doc verification in Task 9

- [ ] **Step 1: Correct EventKind variant count (line 31)**

Find this exact text in the module map code block:
```
    event.rs              ThreadEvent, EventKind (18 variants for event sourcing)
```
Replace with:
```
    event.rs              ThreadEvent, EventKind (20 variants for event sourcing)
```

- [ ] **Step 2: Correct EventKind variant count (line 311)**

Find this exact text in the Event Sourcing section:
```
The `EventKind` enum has 18 variants covering the full lifecycle
```
Replace with:
```
The `EventKind` enum has 20 variants covering the full lifecycle
```

- [ ] **Step 3: Add missing gate/ directory to module map**

Find this exact text in the module map code block (after the `capability/` block):
```
  runtime/                Thread lifecycle management
```
Insert before it:
```
  gate/                   Execution gates (approval, auth, rate limits)
    pipeline.rs           GatePipeline (composes gates in sequence)
    mod.rs                GateDecision, GateResolution, ExecutionMode, ResumeKind
    tool_tier.rs          Tool-tier gate logic
    lease.rs              Lease-gated approval
  runtime/                Thread lifecycle management
```

- [ ] **Step 4: Add executor/llm_stream.rs and executor/orchestrator.rs to module map**

Find this exact text in the executor block of the module map:
```
    prompt.rs             System prompt construction (CodeAct preamble/postamble)
    trace.rs              Execution trace recording and retrospective analysis
```
Replace with:
```
    prompt.rs             System prompt construction (CodeAct preamble/postamble)
    trace.rs              Execution trace recording and retrospective analysis
    llm_stream.rs         LLM streaming support
    orchestrator.rs       Self-modifiable Python execution layer (CodeAct orchestrator)
```

- [ ] **Step 5: Add capability/planner.rs to module map**

Find this exact text in the capability block of the module map:
```
    policy.rs             PolicyEngine (deterministic allow/deny/approve + provenance taint)
```
Replace with:
```
    policy.rs             PolicyEngine (deterministic allow/deny/approve + provenance taint)
    planner.rs            Capability planning logic
```

- [ ] **Step 6: Correct Store trait method count (line 39)**

Find this exact text in the module map:
```
    store.rs              Store trait (20 CRUD methods)
```
Replace with:
```
    store.rs              Store trait (31 CRUD methods)
```

- [ ] **Step 7: Correct Store trait method count (line 267)**

Find this exact text in the HybridStore section:
```
- 20 CRUD methods for threads, steps, events, projects, docs, leases, missions, conversations
```
Replace with:
```
- 31 CRUD methods for threads, steps, events, projects, docs, leases, missions, conversations
```

- [ ] **Step 8: Correct Store trait method count (line 300)**

Find this exact text in the External Trait Boundaries table:
```
| `Store` | 20 CRUD methods for all engine types | `Database` (PostgreSQL + libSQL) |
```
Replace with:
```
| `Store` | 31 CRUD methods for all engine types | `Database` (PostgreSQL + libSQL) |
```

- [ ] **Step 9: Correct ThreadType variants (lines 79-81)**

Find this exact text:
```
- **Interactive** -- user-initiated conversational threads
- **Background** -- scheduled/routine-spawned threads
- **SubThread** -- child threads spawned by a parent thread
```
Replace with:
```
- **Foreground** -- user-initiated conversational threads
- **Research** -- research/analysis threads
- **Mission** -- mission-driven threads spawned by the mission system
```

- [ ] **Step 10: Correct ExecutionMode variants (line 185)**

Find this exact text inside the Execution Gates code block:
```
ExecutionMode:
  Interactive | InteractiveWithAutoApprove | Unattended | Autonomous
```
Replace with:
```
ExecutionMode:
  Interactive | InteractiveAutoApprove | Autonomous | Container
```

- [ ] **Step 11: Verify no stale references remain**

Run Grep on `docs/architecture/ENGINE-V2.md` for each of these stale phrases. Expect zero matches for all:
- `18 variants`
- `20 CRUD`
- `InteractiveWithAutoApprove`
- `Unattended`
- `SubThread`

If any match, fix the remaining occurrence before proceeding.

---

### Task 2: SSH_DELIVERY_MECHANISMS.md — russh version, RSA rationale, OpenCode worker

**Files:**
- Modify: `docs/architecture/SSH_DELIVERY_MECHANISMS.md`

**Interfaces:**
- Consumes: none
- Produces: corrected SSH_DELIVERY_MECHANISMS.md for cross-doc verification in Task 9

- [ ] **Step 1: Correct russh version on line 89**

Find this exact text:
```
Runs a single command on a configured host using the in-process **russh 0.45**
```
Replace with:
```
Runs a single command on a configured host using the in-process **russh 0.62**
```

- [ ] **Step 2: Replace RSA rejection rationale on line 106**

Find this exact text:
```
  (russh 0.45 signs `ssh-rsa` with SHA-1, which modern servers reject).
```
Replace with:
```
  (RSA key support is not implemented; only Ed25519 and ECDSA are accepted).
```

- [ ] **Step 3: Add OpenCode to Mechanism 1 worker examples (line 49)**

Find this exact text:
```
(nanocode/pebble) with `create_job(mode="nanocode", …)`. The worker
```
Replace with:
```
(nanocode/pebble/opencode) with `create_job(mode="nanocode", …)`. The worker
```

- [ ] **Step 4: Add OpenCode to at-a-glance table (line 26)**

Find this exact text in the at-a-glance table:
```
| How the agent invokes it | `create_job(mode="nanocode"…)` with SSH commands |
```
Replace with:
```
| How the agent invokes it | `create_job(mode="nanocode"/"pebble"/"opencode"…)` with SSH commands |
```

- [ ] **Step 5: Add OpenCode to Quick decision guidance (lines 39)**

Find this exact text:
```
- **A whole task on a remote host** (multi-step, long-running, wants container isolation) → **worker mode**.
```
This line is accurate and does not need changing (it already says "worker mode" generically). No edit needed for this step. Mark complete.

- [ ] **Step 6: Verify no stale version references remain**

Run Grep on `docs/architecture/SSH_DELIVERY_MECHANISMS.md` for these stale phrases. Expect zero matches:
- `russh 0.45`
- `SHA-1`

If any match, fix the remaining occurrence before proceeding.

- [ ] **Step 7: Modernize russh_keys crate path in Mechanism 2a description (line 101)**

Find this exact text:
```
  decodes it (`russh_keys::decode_secret_key`) — **credential path (a)**.
```
Replace with:
```
  decodes it (`russh::keys::decode_secret_key`) — **credential path (a)**.
```

- [ ] **Step 8: Verify no stale crate paths remain**

Run Grep on `docs/architecture/SSH_DELIVERY_MECHANISMS.md` for `russh_keys::`. Expect zero matches. This confirms all stale crate paths were modernized.

Run Grep on `docs/architecture/SSH_DELIVERY_MECHANISMS.md` for `russh::keys::`. Expect at least one match (the corrected line). This confirms the modernized path landed.

---

### Task 3: SSH_AGENT_HARNESS.md — Verifier status, component map, data model, version, test counts

**Files:**
- Modify: `docs/architecture/SSH_AGENT_HARNESS.md`

**Interfaces:**
- Consumes: none
- Produces: corrected SSH_AGENT_HARNESS.md consistent with Task 2's SSH_DELIVERY_MECHANISMS corrections

- [ ] **Step 1: Correct HostKeyVerifier status in component map (line 66)**

Find this exact text in the component map table:
```
| `bridge/ssh_hostkeys.rs` | **Host-key verification / known_hosts (in memory).** `HostKeyVerifier` — Strict / AcceptFirst (TOFU), SHA256 fingerprints, mismatch detection. **Built and unit-tested, but not yet wired into a live connection path** (see §7). |
```
Replace with:
```
| `bridge/ssh_hostkeys.rs` | **Host-key verification / known_hosts (in memory).** `HostKeyVerifier` — Strict / AcceptFirst (TOFU), SHA256 fingerprints, mismatch detection. **Live** for the in-process `ssh` and `ssh_git` tool paths (see §7). |
```

- [ ] **Step 2: Add ssh_client.rs to component map**

Find this exact text in the component map table:
```
| `bridge/ssh_api.rs` | **HTTP REST management surface (axum).** CRUD over hosts, key upload/delete/status, agent status/keys. |
```
Insert after it:
```
| `bridge/ssh_client.rs` | **In-process russh client.** `connect_and_exec` used by the `ssh` and WASM `ssh` tools. Wires `HostKeyVerifier` via russh's `Handler::check_server_key`. |
```

- [ ] **Step 3: Add host_key_verifier field to SSHBridge data model**

Find this exact text in the SSHBridge fields list:
```
Fields: `tenant_id: Uuid`, `tenant_name: String`, `hosts:
Arc<RwLock<HashMap<String, SSHHostConfig>>>`, `secrets_store: Arc<dyn
SecretsStore>`, `audit_logger: Arc<dyn AuditLogger>`, `agent_server:
Option<Arc<SshAgentServer>>` (None until started).
```
Replace with:
```
Fields: `tenant_id: Uuid`, `tenant_name: String`, `hosts:
Arc<RwLock<HashMap<String, SSHHostConfig>>>`, `secrets_store: Arc<dyn
SecretsStore>`, `audit_logger: Arc<dyn AuditLogger>`, `agent_server:
Option<Arc<SshAgentServer>>` (None until started), `host_key_verifier:
Arc<HostKeyVerifier>`.
```

- [ ] **Step 4: Add load_key() and host_key_verifier() to key methods list**

Find this exact text:
```
Key methods: `new` (`:338`), `validate` (`:363`), `get_host_config` (`:404`),
`list_hosts` (`:413`), `add_host` (`:419`), `remove_host` (`:438`),
`start_agent_server` (`:464`), `stop_agent_server` (`:546`),
`get_agent_socket_path` (`:554`), `agent_server` (`:561`).
```
Replace with:
```
Key methods: `new` (`:338`), `validate` (`:363`), `get_host_config` (`:404`),
`list_hosts` (`:413`), `add_host` (`:419`), `remove_host` (`:438`),
`start_agent_server` (`:464`), `stop_agent_server` (`:546`),
`get_agent_socket_path` (`:554`), `agent_server` (`:561`), `load_key`
(`:577`), `host_key_verifier` (`:583`).
```

- [ ] **Step 5: Correct russh version and crate path in design decisions (line 297)**

Find this exact text:
```
  (`russh` / `russh-keys` 0.45, `ic/Cargo.toml:153`.)
```
Replace with:
```
  (`russh` 0.62, `ic/Cargo.toml:148`. `russh-keys` is now a module within the `russh` crate, not a separate dependency.)
```

- [ ] **Step 5a: Modernize russh_keys crate paths in component map (ssh_agent.rs row, line 64)**

Find this exact text in the `ssh_agent.rs` component map row:
```
| `bridge/ssh_agent.rs` | **In-process ssh-agent server.** `SshAgentServer` binds the Unix socket, parses keys with `russh_keys::decode_secret_key`, runs `russh_keys::agent::server::serve`, and self-connects to `add_identity` each key into russh's internal keystore. |
```
Replace with:
```
| `bridge/ssh_agent.rs` | **In-process ssh-agent server.** `SshAgentServer` binds the Unix socket, parses keys with `russh::keys::decode_secret_key`, runs `russh::keys::agent::server::serve`, and self-connects to `add_identity` each key into russh's internal keystore. |
```

- [ ] **Step 5b: Verify russh_keys:: references in SSH_DELIVERY_MECHANISMS.md**

The SSH_DELIVERY_MECHANISMS.md line 101 also references `russh_keys::decode_secret_key` — the same stale crate path. This fix is handled in Task 2 Step 7 below to keep all edits to that file in one task.

- [ ] **Step 7: Correct russh version in section 7 (line 326)**

Find this exact text:
```
known_hosts` from its pins. The in-process **russh 0.45 client**
```
Replace with:
```
known_hosts` from its pins. The in-process **russh 0.62 client**
```

- [ ] **Step 8: Correct test coverage total (line 348)**

Find this exact text:
```
21 unit tests, all in-module (`#[cfg(test)]`), none in `ic/tests/`:
```
Replace with:
```
26 unit tests, all in-module (`#[cfg(test)]`), none in `ic/tests/`:
```

- [ ] **Step 9: Add ssh_client.rs row to per-file test table**

Find this exact text in the test table:
```
| `ssh_api.rs` | 1 — `HostRequest` → `SSHHostConfig` conversion |
```
Insert after it:
```
| `ssh_client.rs` | 3 — `append_capped` under-limit, crossing-limit, and already-full output behavior |
```

- [ ] **Step 10: Correct the "Notable gaps" paragraph (lines 360-362)**

Find this exact text:
```
Notable gaps: the agent server is never tested with a **real** key (the sole
test uses an empty map); there are no HTTP-handler tests; and `HostKeyVerifier`,
though well-tested in isolation, has no integration coverage because it is not
wired in.
```
Replace with:
```
Notable gaps: the agent server is never tested with a **real** key (the sole
test uses an empty map); there are no HTTP-handler tests; and `HostKeyVerifier`
is unit-tested and wired into the in-process client path, though live-server
integration test coverage remains an open gap.
```

- [ ] **Step 11: Verify no stale references remain**

Run Grep on `docs/architecture/SSH_AGENT_HARNESS.md` for these stale phrases. Expect zero matches:
- `russh 0.45`
- `not yet wired`
- `no integration coverage because it is not`
- `21 unit tests`
- `russh_keys::`

If any match, fix the remaining occurrence before proceeding.

---

### Task 4: SEMANTIC-MEMORY-SEARCH.md — Provider table and file reference table

**Files:**
- Modify: `docs/architecture/SEMANTIC-MEMORY-SEARCH.md`

**Interfaces:**
- Consumes: none
- Produces: corrected SEMANTIC-MEMORY-SEARCH.md for cross-doc verification in Task 9

- [ ] **Step 1: Add openai_compatible to Supported Providers table**

Find this exact text in the Supported Providers table:
```
| **LunarWing Cloud** | Configurable | Configurable | LunarWing Cloud session auth |
| **Mock** | Deterministic | Configurable | Test harness only |
```
Replace with:
```
| **LunarWing Cloud** | Configurable | Configurable | LunarWing Cloud session auth |
| **OpenAI-compatible** | Any compatible model | Configurable | `EMBEDDING_PROVIDER=openai_compatible`, `OPENAI_API_KEY`; `EMBEDDING_BASE_URL` optional (defaults to `https://api.openai.com`, required for non-default endpoints) |
| **Mock** | Deterministic | Configurable | Test harness only |
```

- [ ] **Step 2: Add workspace/hygiene.rs to File Reference table**

Find this exact text in the File Reference table:
```
| `ic/src/workspace/document.rs` | Core types (MemoryDocument, MemoryChunk, well-known paths) |
```
Insert after it:
```
| `ic/src/workspace/hygiene.rs` | Workspace cleanup/maintenance |
```

- [ ] **Step 3: Verify no stale references remain**

Run Grep on `docs/architecture/SEMANTIC-MEMORY-SEARCH.md` for `openai_compatible`. Expect at least one match (the new row). This confirms the addition landed.

---

### Task 5: XMPP_FILE_TRANSFERS.md — MIME allowlist description

**Files:**
- Modify: `docs/architecture/XMPP_FILE_TRANSFERS.md`

**Interfaces:**
- Consumes: none
- Produces: corrected XMPP_FILE_TRANSFERS.md for cross-doc verification in Task 9

- [ ] **Step 1: Replace the three-bucket MIME description (lines 123-127)**

Find this exact text:
```
The WASM host enforces a MIME-type allowlist. `AttachmentKind` classification:

- `Image` — `image/*`
- `Audio` — `audio/*`
- `Document` — everything else (PDF, text, archives, etc.)
```
Replace with:
```
The WASM host enforces an explicit MIME-type prefix allowlist. Any MIME type
not matching one of these prefixes is rejected.

`AttachmentKind::from_mime_type()` determines the classification for accepted
types. The allowlist and the kind classification are independent decisions:
`image/*` maps to `Image`, `audio/*` maps to `Audio`, and every other allowed
prefix — including `video/*` — maps to `Document`.

| Prefix | Allowed | AttachmentKind |
|--------|---------|---------------|
| `image/` | yes | Image |
| `audio/` | yes | Audio |
| `video/` | yes | Document |
| `application/pdf` | yes | Document |
| `application/vnd.` | yes | Document |
| `application/msword` | yes | Document |
| `application/rtf` | yes | Document |
| `text/` | yes | Document |
| `application/json` | yes | Document |
| `application/zip` | yes | Document |
| `application/gzip` | yes | Document |
| `application/x-tar` | yes | Document |
| `application/octet-stream` | yes | Document |

All other MIME types are rejected outright.
```

- [ ] **Step 2: Verify no stale references remain**

Run Grep on `docs/architecture/XMPP_FILE_TRANSFERS.md` for `everything else`. Expect zero matches. This confirms the old bucket description was removed.

---

### Task 6: SELF_HEAL_DEPLOYMENT_WIRING.md — Timer ownership callout

**Files:**
- Modify: `docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md`

**Interfaces:**
- Consumes: none
- Produces: corrected SELF_HEAL_DEPLOYMENT_WIRING.md for cross-doc verification in Task 9

- [ ] **Step 1: Add explicit ownership callout after the summary section**

Find this exact text at the end of the Summary section:
```
by the `add-tenant` `ensure_health_pipeline()` flow.

## Two distinct "watchdogs" (don't conflate them)
```
Replace with:
```
by the `add-tenant` `ensure_health_pipeline()` flow.

> **Scheduling ownership:** the self-heal pipeline is scheduled exclusively by
> `add-tenant` / `ensure_health_pipeline()`. The `install-lunarwing-watchdog.sh`
> installer copies the self-heal scripts to `/usr/local/sbin` but will never
> schedule them. On a host where the watchdog installer ran but `add-tenant`
> hasn't, the scripts are on disk but unscheduled.

## Two distinct "watchdogs" (don't conflate them)
```

- [ ] **Step 2: Verify the callout landed**

Run Grep on `docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md` for `Scheduling ownership`. Expect exactly one match.

---

### Task 7: ATOMICBOOL_DEEPER_PROPAGATION.md — Broken file reference

**Files:**
- Modify: `docs/architecture/ATOMICBOOL_DEEPER_PROPAGATION.md`

**Interfaces:**
- Consumes: none
- Produces: corrected ATOMICBOOL_DEEPER_PROPAGATION.md for cross-doc verification in Task 9

- [ ] **Step 1: Correct ChatDelegate file reference (line 152)**

Find this exact text in the "Files Likely Affected" list:
```
- `ic/src/llm/delegate.rs` — ChatDelegate emission
```
Replace with:
```
- `ic/src/agent/dispatcher.rs` — ChatDelegate emission
```

- [ ] **Step 2: Verify no stale references remain**

Run Grep on `docs/architecture/ATOMICBOOL_DEEPER_PROPAGATION.md` for `llm/delegate.rs`. Expect zero matches. This confirms the wrong path was removed.

---

### Task 8: WEECHAT-CHANNEL-ARCHITECTURE.md — No changes (verified accurate)

**Files:**
- Review only: `docs/architecture/WEECHAT-CHANNEL-ARCHITECTURE.md`

**Interfaces:**
- Consumes: none
- Produces: confirmation that this file needs no changes

- [ ] **Step 1: Confirm no edits needed**

This file was reviewed and verified accurate in the approved spec. The adapter restart claim and endpoint labels are factually correct. No edits are required.

Mark this task complete. Do not modify the file.

---

### Task 9: Cross-document verification pass

**Files:**
- Read only: all eight files under `docs/architecture/`

**Interfaces:**
- Consumes: all corrected files from Tasks 1 through 8
- Produces: final verification that no stale phrases remain and cross-doc consistency holds

- [ ] **Step 1: Verify russh version is consistent across SSH docs**

Run Grep for `russh 0.45` across all files in `docs/architecture/`. Expected result: zero matches.

Run Grep for `russh 0.62` across all files in `docs/architecture/`. Expected result: matches in SSH_DELIVERY_MECHANISMS.md and SSH_AGENT_HARNESS.md only.

- [ ] **Step 2: Verify HostKeyVerifier status is consistent across SSH docs**

Run Grep for `not yet wired` and `not wired in` across all files in `docs/architecture/`. Expected result: zero matches for both phrases.

The SSH_DELIVERY_MECHANISMS.md already correctly describes the verifier as live (it was accurate before this refresh). The SSH_AGENT_HARNESS.md corrections in Task 3 bring it into agreement.

- [ ] **Step 3: Verify russh crate paths are modernized across SSH docs**

Run Grep for `russh_keys::` across all files in `docs/architecture/`. Expected result: zero matches.

Run Grep for `russh::keys::` across all files in `docs/architecture/`. Expected result: matches in SSH_DELIVERY_MECHANISMS.md and SSH_AGENT_HARNESS.md.

- [ ] **Step 4: Verify no "TBD" or "TODO" was introduced**

Run Grep for `TBD` and `TODO` across all eight files in `docs/architecture/`.

For `TBD`: the ATOMICBOOL_DEEPER_PROPAGATION.md decision log has a pre-existing `- **TBD:**` entry on line 158. This is the original document's text and must not be changed (it records a genuine future event marker, not a placeholder introduced by this work).

For `TODO`: any matches must be pre-existing only. No new occurrences should appear in any file that was edited.

- [ ] **Step 5: Verify ENGINE-V2.md internal consistency**

Run Grep on `docs/architecture/ENGINE-V2.md` for `20 variants` and `31 CRUD`. Each should appear at least twice (module map + event sourcing section for variants; module map + HybridStore and/or trait boundaries for methods).

Run Grep for `18 variants` and `20 CRUD`. Expected: zero matches.

- [ ] **Step 6: Verify all eight files were checked**

Confirm each of these files was either edited or explicitly verified:
1. `docs/architecture/ENGINE-V2.md` -- edited (Task 1)
2. `docs/architecture/SSH_DELIVERY_MECHANISMS.md` -- edited (Task 2)
3. `docs/architecture/SSH_AGENT_HARNESS.md` -- edited (Task 3)
4. `docs/architecture/SEMANTIC-MEMORY-SEARCH.md` -- edited (Task 4)
5. `docs/architecture/XMPP_FILE_TRANSFERS.md` -- edited (Task 5)
6. `docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md` -- edited (Task 6)
7. `docs/architecture/ATOMICBOOL_DEEPER_PROPAGATION.md` -- edited (Task 7)
8. `docs/architecture/WEECHAT-CHANNEL-ARCHITECTURE.md` -- verified, no changes (Task 8)

All eight are accounted for. The refresh is complete.
