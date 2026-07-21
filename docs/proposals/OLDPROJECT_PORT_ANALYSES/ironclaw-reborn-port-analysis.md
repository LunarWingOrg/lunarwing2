# Port IronClaw Reborn Architecture Mechanisms to LunarWing

> **Current status (2026-07-21, upstream `v1.0.0-rc.1` / `82572157`): OPEN / PLANNED.** No selected
> Reborn mechanism from this analysis has landed. Treat this as a selective
> hardening backlog, not a plan to adopt the upstream architecture wholesale.

**Date:** 2026-06-30 (RC range audit appended 2026-07-21)
**Status:** Analysis complete. No code implemented. Selective mechanism backport — **not** a Reborn adoption.
**Kind:** Architecture-level analysis (not a per-release delta). Appends to the `OLDPROJECT_PORT_ANALYSES/` series (0.28.1 / 0.28.2 / 0.29.0 / 0.29.1).

For post-0.29.1 release prioritization, `ironclaw-1.0.0-rc.1-port-analysis.md`
is canonical. This document retains architecture-level rationale and mirrored
mechanism detail; where priority or size differs, use the release-range audit.

## Context

"Reborn" began as IronClaw's ground-up re-architecture from the legacy `src/` monolith into kernel (security perimeter) and userland crates. By `v1.0.0-rc.1`, Reborn is no longer a side beta: upstream deleted the legacy monolith, made the Reborn composition the canonical published `ironclaw`, and removed `crates/ironclaw_engine`. LunarWing forked from IronClaw **≈0.29.x before that cutover**, so it remains the pre-Reborn monolith (`ic/src/` + extracted LunarWing crates). This analysis triages which current-upstream *mechanisms* are worth grafting onto LunarWing, not whether to adopt the upstream architecture wholesale.

**Two framing facts that shape everything below:**

1. **The RC is a real architecture cutover, not a compatibility release.** Commit `b6da0272a` deletes the v1 monolith and cuts deployment to Reborn; `2cc9e4f5f` removes Engine v2. The release notes correctly state that there is no supported in-place 0.29.x upgrade. The internal `ironclaw_reborn_migration` crate is unpublished, excluded from cargo-dist, not wired into the shipped CLI, and explicitly lossy. It is useful as a data-shape reference only.
2. **`lunarwing_engine` is now a permanent LunarWing-owned fork.** Upstream removed `ironclaw_engine` after promoting its useful concepts into kernel contracts (`threads`, `capabilities`, `events`, `turns`, `agent_loop`, `authorization`, `approvals`, and `run_state`). **Consequence:** stop release-diffing `lunarwing_engine` against an upstream counterpart that no longer exists. Mine mechanisms from the current contract crates and adapt them through LunarWing's existing `bridge/` seam.

**Relationship to existing proposals:** the built-in `http` tool's SSRF posture is already documented in `../HTTP_TOOL_SSRF_PROTECTIONS.md` (that path is at parity with Reborn — see Already Present). P1-B below covers a *different* egress path (the sandbox proxy). P1-C overlaps the MULTICA "bare-`*` host allowlist" item in the series `README.md` — see the reconciliation note there.

**Crates analyzed:** `ironclaw_capabilities`, `ironclaw_authorization`, `ironclaw_approvals`, `ironclaw_trust`, `ironclaw_secrets`/`ironclaw_auth`, `ironclaw_network` + `ironclaw_host_runtime/egress`, `ironclaw_events`/`event_projections`/`event_streams`/`reborn_event_store`, `ironclaw_host_api` (ids/scope), `ironclaw_architecture` (boundary tests).

### RC range audited

- **Range:** `ironclaw-v0.29.1` exclusive (annotated tag object `0c3557aa`, peeled commit `556dfd07`) through `ironclaw-v1.0.0-rc.1` inclusive (commit `82572157`, annotated tag object `55010c7e`).
- **Scale:** 1,978 commits, 1,751 non-merge commits, 3,796 changed files. The audit used commit chains, final source, contracts, release notes, and migration tooling rather than changelog text alone.
- **Focus:** security/authorization, secrets, trust and capabilities, sandbox/network egress, runtime/concurrency, migration, and correctness.
- **Result:** Existing P1 sandbox redirects, WASM authority ceilings, and durable events remain valid. The RC audit adds one leading secrets item, two runtime/authority items, and four medium-priority correctness hardenings below.

---

## Changes NOT Applicable to LunarWing (Skip)

| Reborn mechanism | What it is | Why skip |
|---|---|---|
| Multi-crate kernel/userland topology + `ironclaw_reborn_composition` | The whole re-split | No end-user value commensurate with an XL multi-quarter migration. The RC proves the graph can ship upstream, not that replacing LunarWing's established monolith is safer than extracting selected mechanisms. |
| `ironclaw_reborn_migration` | Internal v1/Engine-v2 snapshot importer | Unpublished, cargo-dist-disabled, unwired into the shipped CLI, and intentionally lossy. It does not contradict the release's no-supported-upgrade statement and is not a usable migration path for LunarWing. |
| `AgentLoopDriver` / `DriverRegistry` / `LoopExitApplier` (`ironclaw_turns`, `ironclaw_reborn`) | Hot-swap multiple versioned loop *implementations* behind a trusted exit handshake | LunarWing already has one `run_agentic_loop()` + `LoopDelegate` (Chat/Job/Container) + Engine v2. No need to register/version multiple driver impls. |
| `ironclaw_telegram_v2_adapter`, `ironclaw_slack_v2_adapter`, `ironclaw_product_adapters*` | Proprietary channels behind a normalized product-adapter contract | Charter rejects proprietary channels; LunarWing uses WASM channels + a separate XMPP/OMEMO bridge (structurally different model). |
| `ironclaw_webui_v2*`, `ironclaw_reborn_openai_compat*`, `ironclaw_reborn_webui_ingress` (signed-session) | New browser UI + OpenAI-compat host + OIDC signed session | LunarWing already ships `src/channels/web/` + `src/channels/web/openai_compat.rs`; Reborn versions are duplicative rewrites tied to the composition stack. Attestation story is XMPP/OMEMO, not signed browser sessions. |
| `FirstParty`/`System` runtime lanes + `ironclaw_scripts` (Docker `ironclaw_process_sandbox`) | Host-minted trust lanes + a Docker script-runner lane | LunarWing first-party == native compiled-in code already mediated by `ToolDispatcher`; a Docker process-sandbox lane contradicts the WASM-first, lightweight self-hosted posture. |
| `ExecutionContext::validate()` 7-axis `ResourceScope` coherence check | tenant/user/agent/project/mission/thread/invocation coherence gate | Tenancy in LunarWing is **physical** (per-tenant OS user/PG/port/bridge), so there's no in-process `tenant_id`/`agent_id` axis; `validate()` only pays off once you've built the duplicated-context problem it exists to catch. Port only the `UserId` axis (P2-B). |

---

## Already Present in LunarWing (no port needed)

- **Exact invocation binding and process-local one-shot approvals.** `PendingGate` freezes `{action, call_id, parameters}` behind an unforgeable request UUID; resume cannot supply replacement parameters, and `take_verified` has one in-process winner. This prevents approve-A/execute-B and concurrent replay within one process. It is **not crash-safe exactly-once**: persistent deletion occurs after in-memory removal and deletion failure is ignored, so restart can restore an already-consumed gate. Durable consume/execute settlement belongs in the crash-consistency backlog; a content fingerprint alone does not fix replay.
- **Effect/grant default-deny model** — already exists in `lunarwing_engine` (`EffectType`, `ActionDef.effects`, `CapabilityLease`, `PolicyEngine` Deny>RequireApproval>Allow, default-deny enforcement in `executor/structured.rs`). Don't add a *third* authorization model; converge on this one (P2-D).
- **Secret redaction** — `DecryptedSecret` wraps `secrecy::SecretString` with a `[REDACTED, N bytes]` `Debug`; records hold names/handles only; crypto errors carry no plaintext. Audited: no `.expose_secret()` call site logs a value. Reborn's `[REDACTED]`-Debug pattern buys nothing here.
- **SSRF hardening on the primary `http` tool** — `src/tools/builtin/http.rs` already does DNS-resolve-then-pin (`resolve_to_addrs`), per-hop redirect revalidation, streamed size caps, IPv4-mapped-IPv6 handling, and blocks private IPs unconditionally — at parity with `ironclaw_network`. Documented in `../HTTP_TOOL_SSRF_PROTECTIONS.md`. (The residual sandbox/WASM egress gaps are P1-B and P1-C.)
- **XMPP non-UUID conversation-scope isolation** — already fixed via `scoped_conversation_id()` (stable UUID-v5, length-prefixed) in the 0.29.1 port. Typed IDs would *not* have prevented it (it was a parse-fallback, not type confusion).
- **Autonomous routine-definition mutation denial.** `src/tools/autonomy.rs` already removes `routine_create`, `routine_update`, `routine_delete`, and `routine_fire` from autonomous jobs/routines, with owner-scoped extension filtering and regression tests. Upstream's scheduled-trigger authority ceiling is already represented; retain tests rather than adding another dispatch policy.
- **Project static-file ownership.** The routed handlers are the module-local functions in `src/channels/web/server.rs`, not the similarly named unused duplicates in `handlers/static_files.rs`. They require `AuthenticatedUser`, resolve the project UUID to a sandbox job, compare `job.user_id`, and return not-found on mismatch. Delete/consolidate the dead duplicate handlers, but do not treat this as an IDOR backlog item.

---

## P0 — Security / Correctness

**None.** No analyzed mechanism exposes a LunarWing vulnerability severe enough for P0 (no unauth RCE / silent-auto-approve / plaintext-secret-leak). The security and authority findings below are P1.

---

## P1 — High Priority

### P1-A: Bind encrypted secrets to row identity with versioned AES-GCM AAD
**Upstream source:** `bd6e375af` + row-swap regressions `f5b774127`/`da7dd4866` | **Complexity:** M | **Dependencies:** dual-backend data migration | **Adapt-not-copy**

**Why:** `SecretsCrypto::encrypt(plaintext)` and `decrypt(encrypted_value, salt)` authenticate only the ciphertext. PostgreSQL, libSQL, and the in-memory store pass no owner/name/id context. An attacker with database write access who swaps **both** `encrypted_value` and `key_salt` between two rows under the same master key produces valid decryption under the victim row's name. This does not reveal plaintext directly, but it defeats secret identity and can redirect which credential is injected into a privileged operation. Existing tamper and wrong-salt tests do not cover whole-record transplantation.

**What to change:** Introduce a versioned ciphertext format and canonical, length-prefixed AAD binding at least `{user_id, normalized secret name}`; bind a stable row id too only if upsert identity semantics remain stable. Encrypt new writes with v2 AAD. Decrypt v2 strictly with reconstructed AAD; read legacy v1 only through an explicit compatibility path and re-encrypt transactionally after successful read or via a bounded migration. Never silently retry v2 as v1 after authentication failure. Add cross-user and same-user row-swap tests to all backends.

At the same boundary, stop treating `len >= 32` as proof of entropy: `"a".repeat(32)` currently passes. Require a documented generated-key encoding with at least 256 bits of decoded material, or reject obvious low-entropy/repeated material while retaining a deliberate legacy-key migration path.

**LunarWing files:** `src/secrets/crypto.rs`, `src/secrets/store.rs`, secret schema migrations, setup/key-resolution paths.

### P1-B: Sandbox proxy follows redirects → domain-allowlist bypass / SSRF from worker containers
**Reborn source:** `ironclaw_network/transport.rs` (`redirect::Policy::none()` + revalidate) | **Complexity:** S | **Dependencies:** none (reqwest already dep) | **Adapt-not-copy**

**Why:** `src/sandbox/proxy/http.rs` builds `ProxyState.http_client` with `reqwest::Client::new()` and **no redirect policy** — reqwest 0.12 then follows up to 10 redirects that are **never re-checked against the allowlist**. `decider.decide()` runs once *before* `forward_request`; the `Location` host is never re-validated. This proxy is the **sole egress boundary for untrusted Docker worker containers** (`manager.rs` build_and_start). Net effect: an allowlisted (or container-influenced) host can `302` a container's request to **any** host — including `169.254.169.254` or the operator's private network — and the proxy fetches it and returns the body, carrying any header/query-injected credentials across the redirect. Confirmed trust-boundary bypass + SSRF. Not P0 only because it requires the sandbox enabled + already-sandboxed container code, and it's exfil, not host RCE. Complements the `http`-tool hardening in `../HTTP_TOOL_SSRF_PROTECTIONS.md` — this is the one egress path that lacks equivalent protection.

**What to change:** Set `redirect::Policy::none()` on `ProxyState.http_client`; in `forward_request`, return the upstream `3xx` to the container unmodified (a container re-request re-enters `decider.decide()`) **or** re-run `state.decider.decide()` on the `Location` host before following. This is Reborn's disable+revalidate applied to the one LunarWing egress path lacking it.

**LunarWing files:** `src/sandbox/proxy/http.rs` (client build ~L89, `forward_request` ~L327-441), `src/sandbox/proxy/policy.rs`, `src/sandbox/manager.rs`.
**IronClaw reference:** `crates/ironclaw_network/src/transport.rs` (redirect::none + per-hop revalidate).
**Adaptation:** Orthogonal to IP policy — **do not** import `deny_private_ip_ranges`; `ALLOW_PRIVATE_IPS=1` / TensorZero `192.168.1.157` / local Ollama stay working.

### P1-C: WASM tool credential-injection ceiling — a community tool may exfiltrate a pre-provisioned host secret
**Reborn source:** `ironclaw_trust` `AuthorityCeiling` (the *ceiling* idea, not the type-trick) | **Complexity:** M | **Dependencies:** `trust_level` column already exists (PG+libSQL) | **Adapt-not-copy**

> **Reconciliation / verify-first:** The series `README.md` lists a landed MULTICA "Bare-`*` host allowlist" fix at `capabilities.rs:238`, but the current code still treats bare `*` as match-all in both `HttpCapability::host_matches` (`capabilities.rs`) and `host_matches_pattern` (`credential_injector.rs`, tested at `host_matches_pattern("localhost","*") == true`). Also, `CredentialInjector` **does** gate secrets via `is_secret_allowed` / `allowed_secrets` (used by the builtin `http` tool), but the **WASM host path** `resolve_host_credentials` (`wrapper.rs`) calls `store.get_decrypted(user_id, &mapping.secret_name)` directly and appears **not** to apply that gate. **Confirm both against current `main` before implementing** — the exact residual surface may be narrower than described if later hardening landed.

**Why:** The self-promotion vector Reborn's type-level trick guards **doesn't exist** in LunarWing (skills get trust by source dir with no manifest field; WASM `TrustLevel` isn't `Deserialize`). But the *other half* — trust as an enforced **authority ceiling** — is missing: `TrustLevel` is **vestigial** (only logged at `registry.rs`, never threaded into `to_capabilities()`). Given the two gaps above, a `TrustLevel::User` community WASM tool shipping `capabilities.json` with `credentials{secret_name:"google_oauth_token", host_patterns:["*"]}` + `allowlist host "*"` can inject a **pre-provisioned** OAuth token and reach any host — gated today only by SHA256-of-binary and a non-blocking install printout. Real supply-chain risk for the "self-hoster installs a registry tool" story.

**What to change:** Thread `trust_level` into the capability-build step (`capabilities_schema.rs::to_capabilities`, called from `loader.rs` which currently hardcodes `User`) and **attenuate the self-declared sidecar per level** for `User`-trust tools: (a) reject/strip credential mappings naming a pre-existing host secret the tool didn't provision (esp. OAuth tokens), or require tool-scoped secrets — i.e. apply an `allowed_secrets`-style gate on the WASM host path too; (b) deny wildcard `allowlist`/`host_patterns` `"*"` (force concrete hosts); (c) deny `tool_invoke` aliases resolving to privileged builtins (shell/file_write/http). Reserve unrestricted injection / wildcards for `Verified`/`System` (operator-set in DB). Add an **install-time consent gate** in `src/cli/tool.rs` for sensitive caps instead of print-and-proceed. Skills need no change.

**LunarWing files:** `src/tools/wasm/{storage.rs,loader.rs,capabilities.rs,capabilities_schema.rs,wrapper.rs,credential_injector.rs}`, `src/tools/registry.rs`, `src/cli/tool.rs`.
**IronClaw reference:** `crates/ironclaw_trust/src/decision.rs` (`AuthorityCeiling`), `policy.rs` (fail-closed Sandbox default).
**Adaptation:** Ceiling targets **which secrets + host breadth**, never IP class — a User tool legitimately reaching `192.168.x` must keep working; the `ALLOW_PRIVATE_IPS` branch in `wrapper.rs` is untouched. Single-user self-hosters bump their own tools to `Verified` in DB.

### P1-D: Durable event log + resumable SSE (fixes reconnect drift; adds per-tenant audit trail)
**Reborn source:** `ironclaw_events` (cursor/replay) + `ironclaw_event_streams` (rebase-on-gap) | **Complexity:** L | **Dependencies:** dual-backend migration | **Adapt-not-copy**

**Why:** LunarWing's SSE is fire-and-forget `tokio::broadcast` (buffer 256, lagged clients silently drop); frames set **no `id:`**, `chat_events_handler` reads **no Last-Event-ID/cursor**, and the frontend papers over it with a `loadHistory()` reload that only rebuilds persisted `conversation_messages` — so **ephemeral events (`gate_required`, `approval_needed`, `tool_started`, `thread_state_changed`, `extension_status`) are permanently lost across a missed window.** For an XMPP/mobile-first agent whose SSE drops constantly (sleep/roaming/tunnels), the UI diverges from backend state. LunarWing does persist job actions and Engine V2 events, but lacks a unified durable browser/chat transport-event log with resumable cursors. **Cost is bounded because LunarWing already ships the append-only pattern in `job_events`;** it just is not cursor-parameterized or wired to chat SSE.

**What to change:** (1) Add `chat_events(id monotonic, user_id, thread_id, event_type, data, created_at)` with dual-backend append/list-after operations; the committed id is the cursor. (2) For durable event classes, persist first and broadcast only after commit with that cursor. Define append failure explicitly: log/drop the transport notification or emit a separately classified cursorless transport-only error, but never invent a resumable id. Keep `Heartbeat`/`StreamChunk` explicitly transport-only. (3) On connect, subscribe to live events first, capture the stream head, replay/snapshot through that head, then drain and deduplicate buffered live events by cursor. Read `Last-Event-ID`; emit rebase plus an authoritative snapshot when the cursor is stale. This closes both replay/live gaps and overlap. **Skip** the full projection superstructure; `(user_id, thread_id)` is sufficient. Redact before persistence and broadcast.

**LunarWing files:** `src/channels/web/{sse.rs,server.rs,static/app.js,types.rs}`, `src/db/mod.rs` (+`postgres.rs`/`libsql/`), `migrations/` (+ new `VN__chat_events.sql`), `src/context/memory.rs` / `src/tools/tool.rs` (in-memory `ActionRecord`).
**IronClaw reference:** `crates/ironclaw_events/src/{cursor,sink,error}.rs`, `crates/ironclaw_event_streams/src/manager.rs`, `.claude/rules/gateway-events.md`.
**Note:** Even upstream's *legacy* gateway lacks this — genuinely new capability, argued on its own merits. Not P0 (a dropped `approval_needed` strands the UI, never auto-approves).

---

## P2 — Medium Priority

### P2-A: Reorder credential injection vs. leak-scan on the built-in http tool
**Reborn source:** `ironclaw_host_runtime/egress` scan-then-inject ordering | **Complexity:** S | **Adapt-not-copy**

`src/tools/builtin/http.rs` injects the credential into `headers_vec` **then** leak-scans that same vector. Because `LeakDetector` has **Block** patterns for `github_token`/`google_api_key`/`aws_access_key`/etc., injecting a managed `Authorization: Bearer ghp_…` makes the tool **block its own legitimately-injected credential** (`SecretLeakBlocked → NotAuthorized`). Move the scan to run on **caller-provided URL/headers/body BEFORE injection** — kills the false-positive self-block while preserving exfiltration detection. While here, wire `record_usage()` on the injection paths that currently skip it (`http.rs`, `channels/wasm/wrapper.rs`, `config/mod.rs`) for per-secret audit. *(Fails-closed today, so P2 not P1.)*
**Files:** `src/tools/builtin/http.rs`, `crates/lunarwing_safety/src/leak_detector.rs`, `src/secrets/store.rs`.

### P2-B: Validated `UserId` newtype minted at channel ingress
**Reborn source:** `ironclaw_host_api/src/ids.rs` `validate_scope_id` | **Complexity:** S-M (incremental) | **Adapt-not-copy**

`user_id` is LunarWing's most security-critical axis, it **originates from untrusted channel identity data** at ingress, and flows as a bare `String` through DB scoping, settings, secrets, event routing, and tenant/workspace selection. Add a `UserId` newtype to `lunarwing_common` with channel-aware normalization and validation, then thread it incrementally into `TenantScope::new` and the highest-value DB methods. XMPP bare JIDs legitimately contain `@`, and conversation scopes have broader JID/channel syntax, so do not copy a path-component validator verbatim or apply it to conversation scopes. **Only the user axis** — not `TenantId`/`AgentId` (no in-process analog). Not a big-bang migration.
**Files:** `crates/lunarwing_common/src/lib.rs`, `src/channels/channel.rs`, `src/tenant.rs`, `src/db/mod.rs`.

### P2-C: Stop `lunarwing_engine` `AccessDenied` from leaking cross-user existence
**Reborn source:** "non-existence not authz" convention | **Complexity:** S

15 `EngineError::AccessDenied { user_id, entity }` sites (`runtime/{conversation,manager,mission}.rs`) return a distinguishable authz error on cross-user access — an existence oracle, opposite to both Reborn's convention and LunarWing's own `TenantScope` (which returns `NotFound`). Return a `NotFound`-shaped result instead. `EngineError` does **not** derive `Serialize`, so this is a pure internal refactor (no wire migration). *(Echoes the caller's own `user_id`, so lower severity — the leak is the error type, not an id.)*

### P2-D: Obligations-style grant → WASM `Capabilities` derivation (closes "built tools get empty capabilities")
**Reborn source:** `ironclaw_authorization::obligations_for_grant` (the *shape*) | **Complexity:** M | **Adapt-not-copy**

The documented gap "built tools get empty capabilities; need UX for granting access" (`ic/CLAUDE.md` #5) is exactly what Reborn's obligations pattern solves: port the *shape* of `obligations_for_grant` as a function mapping an approved effect grant → `Capabilities` (`Network⇒http allowlist`, `UseSecret⇒secrets allowlist`, `ReadLocal⇒workspace_read`). Optionally give `lunarwing_engine`'s `CapabilityLease` a `{user_id, thread/project scope}` principal key plus a durable store, with tenant identity implicit in the physically isolated process/workspace. `bridge/store_adapter.rs` already persists some leases to workspace JSON; fix that split-brain rather than adding another store. Add `effects()` to the legacy `Tool` trait so seeded permissions are effect-driven, unifying the two authorization models on one vocabulary. Do **not** port `CapabilityHost`/CAS/spawn-auth.
**Files:** `src/tools/{tool.rs,permissions.rs,wasm/capabilities.rs}`, `crates/lunarwing_engine/src/capability/*`, `src/agent/scheduler.rs`.

### P2-E: Compile the orphaned boundary greps into CI + add a ToolDispatcher-bypass check
**Reborn source:** `ironclaw_architecture` (the *source-grep* style, not the crate-graph test) | **Complexity:** S | **Adapt-not-copy**

The cargo-metadata crate-graph test is **near-vacuous** for a 5-crate monolith. LunarWing already wrote useful DB-driver and `src/llm` checks in `scripts/check-boundaries.sh`, but normal CI does not consistently run them. Move stable rules into `tests/architecture_boundaries.rs`. Scope dispatcher-bypass checks narrowly to **model-initiated capability execution and side-effect paths** that require policy mediation; authenticated control-plane handlers legitimately access stores, workspace, extensions, sessions, jobs, and settings directly. If adding an egress rule, assert use of the policy/proxy boundary, never blanket private-IP rejection.
**Files:** `scripts/check-boundaries.sh`, `scripts/ci/quality_gate*.sh`, new `tests/architecture_boundaries.rs`, `src/tools/dispatch.rs`.

### P2-F (optional): Gate tamper-tripwire
**Complexity:** Trivial. A content digest is only a tamper tripwire: store `sha256(canonical_json(action_name+parameters))`, recompute before execution, and test that resolve paths carry no resume-time parameters. It does **not** make persistent consumption crash-safe. Durable claim/consume/execution settlement needs a separate state transition and recovery test.

### P2-G: Effect-level floors that can never be globally auto-approved
**Upstream source:** RC authorization pre-flight consolidation (`270e6a049`) and capability policy contracts | **Complexity:** S-M

LunarWing has Deny/Ask/Allow settings, but operator convenience settings should not be able to erase every approval boundary for irreversible or authority-changing effects. Add a small effect-level floor in the existing policy engine: selected effects such as credential export, authorization/policy changes, automation-definition mutation, destructive external writes, and financial/signing operations remain deny or per-invocation approval regardless of a global auto-approve preference. Keep the list narrow and explicit; do not add another policy system.

**Files:** `src/tools/permissions.rs`, `crates/lunarwing_engine/src/capability/`, audited dispatch context.

### P2-H: Read-before-edit, stale-content fingerprints, and per-path write serialization
**Upstream source:** `dd579bd61` | **Complexity:** M

LunarWing's native file-edit/write path lacks a general optimistic-concurrency contract. Require an observed file version/content digest for destructive edits, reject stale writes, and serialize mutations per canonical path. This prevents concurrent agent turns or tools from silently overwriting each other's changes and turns "read before edit" from prompt advice into a host invariant. Preserve explicit create/replace operations where the caller intentionally chooses those semantics.

**Files:** `src/tools/builtin/file.rs`, workspace filesystem abstraction, tool-dispatch execution metadata.

### P2-I: Compaction no-progress circuit breaker
**Upstream source:** `36d5495d3` plus RC loop-resilience work | **Complexity:** S-M

`ContextCompactor::compact` reports token counts but does not require material reduction, and auto-compaction logs a failure then continues. Track before/after prompt identity and token reduction across attempts. If compaction repeats without progress, stop retrying, emit an honest model/user-visible failure, and fall back only to an explicit bounded truncation policy. This avoids cost loops and repeated prompt-cache breaks while preserving LunarWing's current archival-before-truncate safety.

**Files:** `src/agent/compaction.rs`, `src/agent/thread_ops.rs`, context-monitor state.

### P2-J: Durable, idempotent extension and OAuth cleanup
**Upstream source:** `67662414d`, `4a256f8ef`, `195fab979`, and `ec60077fd` | **Complexity:** M

`ExtensionManager::remove` starts by clearing in-memory pending-auth/OAuth state, then performs a sequence of registry, process, tool, config, and credential cleanup operations. A crash or mid-sequence error can leave a partially removed extension with no durable cleanup intent. Persist a cleanup state/operation id, make each step idempotent, retry incomplete cleanup at startup, and linearize remove versus OAuth callback/refresh. Do not report removal complete until required credential and route revocation has committed.

**Files:** `src/extensions/manager.rs`, OAuth flow persistence, extension registry/config stores.

---

## Recommended Implementation Order

```
Security first, cheap-and-confined first:
1.  P1-A  Versioned row-bound secret AAD + key validation         [M]  security; migration design first
2.  P1-B  Sandbox proxy: disable+revalidate redirects              [S]  security, confined
3.  P2-A  Reorder http-tool leak-scan before injection             [S]  fixes self-block; trivial
4.  P1-C  WASM credential-injection ceiling + install consent       [M]  supply-chain; reconcile MULTICA
5.  P2-G  Never-auto-approve effect floors                         [S-M] defense in depth
6.  P2-C  Engine AccessDenied -> non-existence                      [S]  internal, no wire migration
7.  P2-B  Validated UserId newtype at ingress                      [S-M] forward hardening
8.  P2-H  Read-before-edit + stale fingerprints                    [M]  concurrent correctness
9.  P2-I  Compaction no-progress circuit breaker                  [S-M] runtime/cost safety
10. P2-J  Durable idempotent extension/OAuth cleanup                [M]  lifecycle correctness
11. P1-D  Durable chat_events log + resumable SSE                    [L]  correctness + audit
12. P2-D  Grant->Capabilities derivation                            [M]  unify one effect vocabulary
13. P2-E  Boundary greps -> cargo test + dispatch-bypass check       [S]  CI wiring
14. P2-F  Gate tamper-tripwire + no-resume-params test         [Trivial] optional
```

## Verification

- Per item from `ic/`: `taskset -c 0-5 cargo fmt --all -- --check`; targeted `taskset -c 0-5 cargo test -j6 <test> -- --test-threads=6`; relevant `taskset -c 0-5 cargo check -j6` feature combinations; `scripts/pre-commit-safety.sh`. Run broader clippy/test matrices only when the implementation scope warrants them. Every fix needs a regression test (commit-msg hook enforced).
- **P1-A:** swapping `{encrypted_value,key_salt}` between same-user and cross-user rows fails authentication under both backends; legacy rows migrate once and v2 authentication failure never falls back to v1.
- **P1-B:** test that a `302` from an allowlisted host to an off-allowlist host is **not** fetched by the proxy.
- **P1-C:** test that a `User`-trust tool declaring `host:"*"` + `google_oauth_token` injection is capped/denied; that a `Verified` tool is not. (First re-confirm the residual surface against current `main`.)
- **P1-D:** reconnect with `Last-Event-ID` replays missed `gate_required`/`approval_needed`; a stale cursor triggers rebase; dual-backend parity.
- **P2-A:** a `ghp_`-shaped secret injected as an http credential is **not** blocked, but a `ghp_` value in a caller-supplied body **is**.
- **P2-H:** two edits based on the same file digest have one winner and one stale-write error.
- **P2-I:** identical/no-smaller compaction output trips a bounded no-progress result instead of retrying indefinitely.
- **P2-J:** injected failure after each removal step converges to fully removed state on retry/startup.

---

**Net read:** The RC invalidates the old assumption that Reborn is an unfinished side binary, but it does not make a wholesale architecture transplant sensible. The payoff remains selective hardening. The leading issue is row-bound authenticated encryption (P1-A), followed by the sandbox redirect bypass (P1-B) and WASM authority ceiling (P1-C). Autonomous routine mutation denial is already present. Bounded aggregate WASM admission is a concrete P1/P2 availability item documented in `../IRONCLAW_ADDITION_CANDIDATES.md`, backed by upstream's starvation incident. Everything is adapt-not-copy; network changes must preserve intentional `ALLOW_PRIVATE_IPS` deployments.
