# IronClaw 0.29.1 to 1.0.0-rc.1 Port Analysis

> **Status (2026-07-21): ANALYSIS COMPLETE / SELECTIVE BACKLOG.** This is a
> release-range audit, not a proposal to migrate LunarWing to IronClaw's new
> architecture. No code change is implied.

## Scope and method

- **Upstream range:** `ironclaw-v0.29.1` commit `556dfd07789c8230c5241b64f4b7c9d549589b10` (exclusive) through `ironclaw-v1.0.0-rc.1` commit `8257215700fd75a3636338e969605f5dee8f99c4` (inclusive).
- **Scale:** 1,978 commits, 3,796 changed files, about 1.30 million insertions and 157,000 deletions.
- **LunarWing baselines:** current v2 `61d0c1e` (`LunarWing_v2/`) and current v1 staging `20489a35` (`lunarwing/`).
- **Prior work checked:** all analyses in this directory plus `../IRONCLAW_ADDITION_CANDIDATES.md`.
- **Method:** inspect final source, focused commit chains, tests, migrations, release notes, and the current LunarWing implementations. Changelog entries alone were not treated as proof.

The tag interval includes commits whose author dates predate the 0.29.1 release because upstream developed Reborn on a parallel branch and later merged it. The meaningful comparison is therefore the reachable tag-to-tag tree and history, not a simple date window.

## Executive conclusion

IronClaw 1.0.0-rc.1 is an architecture cutover, not an incremental 0.29.x upgrade. Reborn became the published `ironclaw` binary, the legacy monolith was removed, and `ironclaw_engine` was deleted. Upstream explicitly provides no supported in-place migration for 0.29.x config, databases, settings, or secrets. The internal migration crate is unpublished, unwired into the shipped CLI, and intentionally lossy.

LunarWing should **not** follow that cutover. Its monolith, physical tenant isolation, Engine V2 bridge, XMPP/OMEMO channels, workspace memory, jobs, routines, OpenRC/systemd-user operations, and Kawarimi flow have all diverged in useful ways. The correct strategy remains selective mechanism ports.

The release range does contain worthwhile additions that were not in the prior LunarWing backlog:

1. Bind encrypted secret ciphertext to its row identity with AES-GCM AAD.
2. Stop using reserved browser SSE event name `error` for application errors.
3. Make browser chat submission idempotent across retries and lost acknowledgements.
4. Add a no-progress circuit breaker for repeated ineffective compaction.
5. Enforce read-before-edit with stale-content rejection for native file tools.
6. Make extension OAuth/removal cleanup durable and restart-convergent.
7. Add crash-consistency tests around claims, gates, terminal state, and idempotency.
8. Add fail-loud persistent-state/deployment-profile startup validation.
9. Add cheap CI guards for hermetic environment mutation and embedded Docker assets.
10. Reopen the OpenAI Responses API as a deliberate product project, not a patch port.
11. Expose routine gate/auth holds and stop hiding schedule-computation errors.

Several strong findings from the range were already documented: WASM channel HTTP hardening, sandbox redirect SSRF, WASM credential authority ceilings, resumable SSE, bounded WASM work, type-safe IDs/statuses, and architecture checks. They remain valid but are not new findings here.

## LunarWing v1 versus v2 applicability

The two LunarWing lines still share most of the pre-Reborn host surfaces, so
several findings apply to both rather than being v2-only:

| Candidate | LunarWing v1 | LunarWing v2 | Notes |
|---|---|---|---|
| N1 secret row binding | Applies | Applies | Both use the same no-AAD `SecretsCrypto` shape |
| N2 reserved SSE error | Applies | Applies | Both emit `event: error` and register transport plus named error handlers |
| N16 send idempotency | Applies | Applies | Both request DTOs lack a client action ID and browser retry resubmits |
| N3 schedule/hold correctness | Applies | Applies | Both hide some `next_cron_fire` errors; v2 has deeper Engine gate state to project |
| N4 compaction breaker | Applies | Applies, including Engine paths | Implement per runtime rather than assuming one shared loop |
| N5 stale file edits | Applies | Higher value | v2 has more concurrent Engine/external-worker activity |
| N6 extension cleanup | Applies | Applies | Both removal paths are multi-step and primarily in-memory |
| N8 backup/state completeness | DB/workspace concern | Higher value | v2 Engine runtime state lives in workspace files outside relational history |
| N10 Responses API | Stub/absent | Stub/absent | Product decision for v2; no reason to add independently to maintenance-only v1 |

V2-specific advances already reduce the value of several upstream ports: it
has native provider streaming, in-flight cancellation, persisted Engine events,
restart-persistent pending gates, self-evolving skills, and managed stdio MCP.
Those features do not make browser SSE resumable and do not make a DB-only
backup complete. V1's direct Codex/ChatGPT session provider is absent from v2,
but nothing in this IronClaw range justifies restoring it instead of using the
current OpenAI-compatible/provider-router path.

## Priority summary

| ID | Recommendation | Priority | Size | Prior status |
|---|---|---:|---:|---|
| N1 | Row-bound authenticated encryption for secrets | P1 | L | New |
| N2 | Rename application SSE `error` to `stream_error` | P2 | S | New |
| N16 | Browser chat send idempotency | P1/P2 | M | New |
| N3 | Routine hold visibility and scheduler correctness | P2 | M | New |
| N4 | Compaction no-progress circuit breaker | P1/P2 | S-M | New |
| N5 | Read-before-edit and stale-write rejection | P1/P2 | M | New |
| N6 | Durable extension OAuth/removal cleanup | P1/P2 | L | New |
| N7 | Crash-consistency failure matrix | P2 | M-L | Sharpened from generic backlog |
| N8 | Persistent-state/profile startup guards | P2 | M | New |
| N9 | CI reliability guard bundle | P2 | S-M | New |
| N10 | OpenAI Responses API | P2 product project | L-XL | Reopened |
| N11 | One-shot routines and structured schedule errors | P2 | M | New |
| N12 | Operator log correlation | P2 | M | New refinement |
| N13 | Kawarimi loss manifest and dry-run parity | P2 | M | New mechanism |
| N14 | Progressive skill disclosure | P2/P3 | S-M | New refinement |
| N15 | Provider retry classification audit | P2/P3 | M | New comparison |

## New high-value candidates

### N1: Bind secret ciphertext to owner and name

**Upstream:** `bd6e375af`, with row-swap regressions in `f5b774127` and `da7dd4866`.

**Current LunarWing:** `ic/src/secrets/crypto.rs` derives a key from the master key and random salt, then encrypts without AAD. `encrypt()` receives only plaintext; `decrypt()` receives only ciphertext and salt. PostgreSQL, libSQL, and in-memory rows therefore do not cryptographically bind ciphertext to `user_id` or secret name.

**Concrete failure:** a party able to modify the secrets database can transplant both `encrypted_value` and `key_salt` from one row to another under the same master key. Authentication still succeeds, but the victim row now resolves to a different valid secret. This can redirect which OAuth token/API key is injected into a privileged host operation. AES-GCM prevents bit tampering, not whole-record substitution unless row identity is authenticated as AAD.

**Port shape:** introduce a versioned ciphertext envelope and canonical length-prefixed AAD containing at least normalized `{user_id, secret_name}`. Encrypt all new writes as v2. Read legacy v1 only through an explicit compatibility path and transactionally re-encrypt after successful decryption or via a bounded migration. Never fall back from failed v2 authentication to v1.

The same upstream review found that length-only master-key validation accepts low-entropy values. LunarWing explicitly tests that 32 repeated `a` bytes are valid. Do not attempt unreliable entropy estimation. Prefer a documented generated encoding with at least 256 bits for new installs, reject obvious placeholder/repeated values, and retain an explicit migration path for existing deployed keys.

**Migration risk:** high enough to design first. Kawarimi, backup/restore, keychain loading, environment keys, both DB backends, and existing tenant secrets must remain readable during a controlled transition.

**Tests:** same-user and cross-user row swaps fail; v1 read upgrades exactly once; v2 authentication failure never downgrades; backup/restore retains envelope version; both backends agree on AAD normalization.

### N2: Do not emit application failures as SSE `event: error`

**Upstream:** `bf67f0289`, primarily `ironclaw_webui` SSE handlers and frontend reconnect state.

**Current LunarWing:** `ic/src/channels/web/sse.rs` maps `SseEvent::Error` to `"error"`. `ic/src/channels/web/static/app.js` installs both `eventSource.onerror` for transport failure and `addEventListener('error', ...)` for application failure.

**Concrete failure:** `error` is the browser `EventSource` transport-error event. An application frame named `error` can enter both the message path and connection-failure handling, displaying a false disconnect/reconnecting state after a valid server-sent error. It can also reset stream UI while the browser is still connected.

**Port shape:** emit `stream_error` (or `application_error`) and update the named listener. Keep `onerror` exclusively for native transport state. Add a browser-level regression proving an application failure does not increment reconnect attempts or mark the connection disconnected.

This is independent of, but should precede, the larger durable SSE work.

### N16: Make browser chat submission idempotent

**Upstream:** `9b913f138`, `26fd07304`, and real-ledger coverage in `62bfdf9a2`.

**Current LunarWing:** `SendMessageRequest` has no client action/idempotency key. `chat_send_handler` creates a fresh `IncomingMessage` UUID on every POST. The frontend retry link restores the text and calls `sendMessage()` again, producing a new request with no relation to the original.

**Concrete failure:** the server can enqueue the first message, then the HTTP acknowledgement can be lost. The UI marks the send failed and retrying enqueues the same user action again. Both turns can execute tools or external side effects.

**Port shape:** generate a stable `client_action_id` before the first send and retain it across retries; persist an acknowledgement ledger keyed by authenticated user plus action ID; atomically record/enqueue or otherwise make the enqueue boundary replay-safe; return the original acknowledgement for duplicates without dispatching again. Bind the stored request digest so reusing an ID with different content is rejected. Define retention and cleanup explicitly.

This should cover REST first and then WebSocket/other browser ingress if they expose the same retry ambiguity.

### N3: Explain held schedules and audit atomic due-fire creation

**Upstream:** `1a56f2878` (gate/auth hold visibility), `c62f0f841` (terminal handling for failed one-shot triggers), and `d2b919648` (structured trigger input errors). Upstream `ef729fc00` is a non-porting comparison because LunarWing has no equivalent durable pre-run claim.

**Current LunarWing:** `check_cron_triggers()` reads due routines and calls `spawn_fire()`; the spawned task then creates the `RoutineRun`. There is no durable claim to become permanently wedged: a crash before insertion leaves the routine due. Routine views do not expose a derived active-hold reason or number of elapsed schedule slots.

**Concrete failures:**

- A routine parked for approval or authentication appears merely inactive/stuck.
- Single-flight scheduling silently skips later cron occurrences while the first run remains held.
- `next_cron_fire(...).unwrap_or(None)` converts schedule/timezone errors into “no next run,” hiding configuration defects.
- The read-due then spawn/create sequence is not an atomic fire identity boundary; if more than one poller/process can observe the same database, duplicate eligibility is possible.

**Port shape:** derive, rather than persist, an optional hold projection `{reason, since, elapsed_occurrences, capped}` from active run/job/gate state. Surface schedule errors rather than mapping them to `None`. Separately decide whether LunarWing supports multiple pollers against one tenant DB; if so, introduce an atomic occurrence identity/insert or lease appropriate to its schema. Do not import upstream stale-claim recovery without first adding a claim state.

**Already present:** autonomous jobs/routines cannot mutate routine definitions because `src/tools/autonomy.rs` denies `routine_create`, `routine_update`, `routine_delete`, and `routine_fire`. Do not add a duplicate authority layer.

### N4: Stop ineffective automatic compaction loops

**Upstream:** `36d5495d3` and `efcc6c703`.

**Current LunarWing:** `ContextCompactor` calculates `tokens_before` and `tokens_after`, but callers do not maintain a consecutive ineffective-compaction breaker. A summary can be too large, archival failure can preserve all turns, or the retained tail can remain above the trigger threshold.

**Concrete failure:** automatic compaction immediately retriggers, repeatedly spending model tokens and breaking prompt caches without reducing context enough to proceed.

**Port shape:** classify automatic compaction as effective only after the new prompt is assembled and materially below the prior size/threshold. After a small number of consecutive ineffective automatic attempts, open a per-thread breaker. Forced context-overflow recovery may bypass it once under a separate bounded budget. Reset after meaningful shrinkage or new context conditions.

Do not replace LunarWing's important archive-before-truncate behavior. An archive write failure should preserve turns and count as ineffective, not silently discard history.

### N5: Enforce read-before-edit and reject stale writes

**Upstream:** `dd579bd61`.

**Current LunarWing:** native file read/write/patch tools have path validation but no general optimistic-concurrency token proving the caller edited the version it read.

**Concrete failure:** a user, another turn, or another agent can modify a file between model read and edit. The later edit silently overwrites newer work. Physical tenant isolation does not help because the race is inside one tenant workspace.

**Port shape:** return a stable version/content digest from reads; require it for patch and destructive replacement; scope read evidence by user, run/thread, and canonical path; serialize mutations per canonical path; return a recoverable stale-write error instructing the model to reread. New-file creation and intentional force-replace need explicit semantics rather than accidental bypasses.

This is especially useful for LunarWing's multi-agent/external-worker workflows and cross-machine editing pattern.

### N6: Make extension lifecycle cleanup restart-convergent

**Upstream:** `195fab979` and `486edf161`.

**Current LunarWing:** `ExtensionManager::remove()` clears in-memory pending auth first, then unregisters tools, removes clients/processes, updates config, revokes mappings/hooks, persists active channels, and deletes files. Several cleanup results are deliberately discarded. A crash or error midway can leave a partially removed extension without durable cleanup intent.

**Concrete failures:**

- OAuth callback succeeds but activation continuation is lost on restart.
- Removal clears pending state, then fails before credentials/routes/files are revoked.
- A callback or refresh races removal and recreates state.
- Package state is deleted before externally required cleanup completes.
- Startup cannot distinguish complete removal from interrupted removal.

**Port shape:** persist a lifecycle operation ID and required cleanup obligations before side effects; make each step idempotent; claim continuation dispatch with a bounded lease/fence; retry incomplete operations at startup; settle removal only after required credential, route, process, and installation cleanup commits. Keep package installation ownership separate from per-user activation.

This is not a direct copy of Reborn's large lifecycle graph. Start with removal and OAuth continuation because those are the current LunarWing failure boundaries.

## New reliability and operations candidates

### N7: Persistence crash-consistency matrix

**Upstream:** `8a8c58d29` and related durable-turn fault injection.

The useful mechanism is a deterministic crash/reopen oracle, not Reborn's row-store architecture. Add a smaller PostgreSQL-first LunarWing matrix covering:

- crash after routine-run insertion but before execution/terminal settlement;
- crash after pending gate persistence;
- side effect completed but terminal write missing;
- same idempotency key after restart;
- terminal run no longer retaining an active thread/job lock;
- failure reason preserved after restart;
- replay does not execute an approved action twice.
- crash after in-memory gate consumption but before durable deletion does not restore an executable approval.

Current stuck-run tests cover timeout selection and finalization, not arbitrary acknowledged-write crash points.

### N8: Fail loudly on ephemeral or mismatched production state

**Upstream:** `7ade42f45`, `0c79a2d28`, `9ac7b476c`, and container entrypoint guards.

Adapt this to LunarWing's actual deployment model through `ic/scripts/lunarwing-mt-admin.sh`, not a new service manager. Startup/provision verification should reject:

- tenant state paths on ephemeral storage;
- restored DB/workspace ownership mismatches;
- imported owner scope that does not match the target tenant;
- multi-tenant profiles using single-node ports/defaults;
- expected port registry/service state outside durable locations;
- a DB-only backup presented as a complete Engine V2 backup when `engine/.runtime/` workspace state is absent.

Keep systemd-user and OpenRC first-class. Upstream's systemd/launchd service manager is not suitable for LunarWing.

### N9: Cheap CI reliability guards

**Upstream:** `d3c5d2489`, `c41896a65`, `65e3f98c2`, and `85c02c29f`.

Recommended bundle:

1. Add a delta-scoped check that new `std::env::set_var/remove_var` test sites use the shared environment lock/RAII restoration. LunarWing has many existing sites, so a big-bang denial would create noise.
2. Validate that every production `include_str!` input is present in every Docker builder context that compiles its crate. `ic/Dockerfile.test` has narrower `COPY` coverage than the main Dockerfile and is a plausible drift point.
3. Set workspace `unused_must_use = "deny"`, then separately audit deliberate `let _ = Result` sites. The lint alone does not catch explicit discards.
4. Add production-composition versus test-harness parity checks around `AppBuilder`, Engine V2 routing, dispatcher/policy wrappers, and persistence.

Do not mechanically turn every best-effort send/shutdown into a fatal error. Classify persistence, credential revocation, lifecycle settlement, and notification delivery separately.

### N10: Implement OpenAI Responses API as a product project

**Upstream chain:** contracts `b51e5699d`, workflow `36ce10a91`, SSE translation `4c185e675`, models/validation `7e191be3f`, external-tool continuation `2fa0cb46b`, usage/cost `4a53cf5c1`, and inline images `31eacd406`.

The old 0.29.1 analysis correctly skipped temperature plumbing because LunarWing's endpoint was a stub. The situation has changed: upstream now has a complete tested implementation, while `ic/src/channels/web/responses_api.rs` is still a four-line unregistered stub.

This is worth including only if LunarWing wants standard client interoperability beyond Chat Completions. Implement against LunarWing's runtime rather than importing ProductWorkflow/Reborn types. Suggested phases:

1. `/v1/responses`, text/content parts, bearer auth, limits, model selection, usage.
2. Real streaming event translation and cancellation.
3. Existing LunarWing image attachment integration.
4. External-tool continuation only after response/run identity and persistence are designed.

The existing `/v1/chat/completions` route remains useful and should not be destabilized by this work.

### N11: One-shot routines and honest schedule errors

**Upstream:** `726bbb3de` and `c62f0f841`.

A `run once at <timestamp>` schedule is useful and simpler than creating a cron expression plus cleanup. If added, permanently failed one-shot routines must become terminal rather than repeatedly eligible. Validate timezone at creation, persist an unambiguous instant, and expose parse failures to the user/model.

Do not rename LunarWing routines to “automations” solely for parity.

### N12: Correlate operator logs by tenant, thread, and run

**Upstream:** `570649a35`, `4622c9cf2`, and `6e4b04464`.

LunarWing already broadcasts and downloads logs. The missing operator value is stable correlation and filtering across concurrent activity. Add structured tenant/user, thread, run/job, component, and time-range fields, while keeping secret redaction. In multi-tenant operations, expose this through existing admin/health abstractions rather than bare service commands.

### N13: Kawarimi dry-run parity and loss manifest

**Upstream reference:** internal migration engine `209da7d34`.

Do not port the converter. Port its reporting discipline to Kawarimi:

- machine-readable converted/skipped/degraded counts;
- record identifiers without secret values;
- dry run predicts the same loss set while writing nothing;
- reruns are idempotent;
- missing expected tables are errors, not silently “empty data”;
- post-import readback uses production APIs;
- DB and Engine V2 workspace coverage are reported separately.

This supplements the verified v1/v2 SQL migration compatibility. It addresses operational completeness rather than a migration-schema incompatibility.

## Lower-priority refinements

### N14: Progressive skill disclosure

**Upstream:** `ae553971b` and `ee838183d`.

LunarWing already has deterministic skill selection and a richer Engine V2 skill-evolution flow. Only adopt the prompt optimization: compact stable one-line skill listings in base context, full `SKILL.md` content only after selection, deterministic ordering for prompt-cache stability. Verify current prompt assembly first; do not port the framework.

### N15: Provider retry classification audit

**Upstream:** `bf85a8c99`.

LunarWing already has retry, timeout, failover, circuit breaking, smart routing, and cancellation. Compare error classification rather than copying code:

- retry transient availability with cancellation-aware backoff;
- fail immediately for missing provider/credentials or invalid configuration;
- preserve actionable model-visible tool errors;
- keep retry ceilings separate from loop iteration limits;
- do not retain partial failed streams in cache/recordings.

## Already documented and still valid

| Existing item | Current evidence | Source document |
|---|---|---|
| WASM channel redirect/private-target hardening | Channel client still lacks tool-side parity | `../IRONCLAW_ADDITION_CANDIDATES.md` #1 |
| WASM channel leak-scan ordering | Channel header placeholders are scanned after substitution | Addition candidates #2 |
| Sandbox proxy redirect SSRF | Proxy client still follows redirects without destination revalidation | Reborn P1-B |
| WASM credential authority ceiling | User-trust wildcard/credential scope remains broad | Reborn P1-C |
| Durable chat events and resumable SSE | Broadcast buffer 256, no `id`, no cursor/replay | Reborn P1-D |
| UTF-8-safe CLI truncation | Byte slicing remains in MCP/config display paths | Addition candidates #3 |
| Typed `JobResultStatus` | Shared/web contracts still use `String` | Addition candidates #4 |
| Narrow `ExternalThreadId`/`UserId` | Channel/user boundaries remain raw strings | Addition candidates #5 and Reborn P2-B |
| Bounded aggregate WASM work | `spawn_blocking` exists, shared admission bounds do not | Addition candidates #6 |
| Wasmtime support/advisory audit | LunarWing remains on 36.0.12 | Addition candidates / 0.29.0 analysis |
| Embeddings metadata-target hardening | Must preserve intentional private endpoints | 0.29.0 analysis |
| CodeAct operator kill switch | No `LUNARWING_DISABLE_CODEACT` equivalent found | 0.29.0 analysis |
| Architecture boundary checks | Existing scripts are not consistently normal-CI enforced | Reborn P2-E |

The reserved SSE event rename (N2) is a small prerequisite, not a substitute for durable replay.

## Already present in LunarWing

- **Autonomous routine mutation denial:** enforced in `src/tools/autonomy.rs`.
- **Project static-file ownership:** routed handlers in `src/channels/web/server.rs` require `AuthenticatedUser`, resolve the project UUID to its sandbox job, and return not-found on owner mismatch. Similarly named duplicate handlers elsewhere should be consolidated, not treated as an IDOR.
- **Exact one-shot approval consumption:** frozen `PendingGate` payloads plus atomic `take_verified()` and restart persistence.
- **Native Engine V2 streaming and cancellation:** v2 now preserves streams through provider decorators and uses per-thread cancellation tokens.
- **Attachments and vision:** web/channel attachment ingestion, extracted text, previews, and multimodal plumbing already exist.
- **Model selection and turn cost:** selected-model controls, usage, and cost events already exist.
- **Onboarding/keychain:** LunarWing has provider/channel setup, keychain master-key support, and DB-backed settings/secrets layering.
- **Service management:** LunarWing's systemd-user/OpenRC/tenant-aware admin tooling is more applicable than upstream's service CLI.
- **Workspace memory:** documents, versions, chunking, embeddings, FTS/vector search, identity files, and hygiene already exceed the useful subset of upstream memory changes.
- **Skill evolution:** proposals, evidence, approval/rejection, versions, rollback/demotion/archive/prune are already present in Engine V2.
- **Dependency fixes:** LunarWing already has the audited PostgreSQL crate versions, `crossbeam-epoch 0.9.20`, and `serde_norway`.
- **Non-UUID conversation isolation:** the 0.29.1 UUID-v5 scope fix is present in both current LunarWing lines.

## Explicit skips

| Upstream change | Decision | Reason |
|---|---|---|
| Whole Reborn crate/kernel topology | Skip | XL rewrite with no proportional LunarWing user value |
| Reborn binary/home/config cutover | Skip | Fresh-install upstream contract conflicts with LunarWing continuity and Kawarimi |
| Internal Reborn migration crate | Skip code | Unpublished, unwired, lossy; mine reporting ideas only |
| RootFilesystem migrations V28-V32 | Skip | Coherent only with upstream storage fabric; unsafe as ordinary LunarWing schema additions |
| Remove Engine V2 | Do not follow | `lunarwing_engine` is now LunarWing-owned and actively extended |
| Remove jobs/missions/memory/MCP CLI surfaces | Do not follow | LunarWing has real users and richer semantics for these surfaces |
| Slack/Telegram/product adapters | Skip | Proprietary-channel policy and mismatched XMPP/WASM architecture |
| Reborn WebUI wholesale | Skip | Port isolated correctness behavior only |
| Full in-process multi-tenant directory/scope graph | Skip | Production tenancy is physical per OS user/process/database |
| Root filesystem/mount fabric | Skip | Duplicates LunarWing workspace and DB abstractions |
| Full authorization/approval graph | Skip | Existing gate and Engine policy models are architecture-appropriate |
| Trace Commons | Skip | Hosted contribution surface conflicts with self-host/privacy priorities |
| systemd/launchd service manager | Skip | Omits OpenRC and tenant-user manager requirements |
| Release packaging/i18n churn | Deprioritize | Product-specific and unrelated to a concrete LunarWing gap |

## Recommended order

1. Existing WASM channel HTTP/leak-scan fixes and sandbox redirect SSRF.
2. Existing bounded WASM prepare/execute admission limits.
3. N1 row-bound secret AAD design and migration plan; implement only after tenant compatibility tests exist.
4. N16 browser send idempotency.
5. N2 reserved SSE event rename as a quick independent correctness fix.
6. N3 schedule error handling, hold visibility, and atomic-fire audit.
7. N4 compaction breaker and N5 stale-edit protection.
8. N9 CI guard bundle.
9. N7 crash-consistency matrix, including durable pending-gate consumption.
10. N6 durable extension/OAuth cleanup.
11. Existing durable SSE replay design, incorporating N2 frontend lessons.
12. N8 startup/profile/state completeness checks and N13 Kawarimi reporting.
13. N10 Responses API as an independently scoped product project.
14. N11-N15 according to user demand and production evidence.

## Verification guidance

Documentation-only verification for this audit:

- exact tag commits and range count recorded;
- candidate claims checked against current v1 and v2 source;
- existing backlog cross-referenced to prevent duplicate items;
- upstream commits checked in the requested tag ancestry;
- `git diff --check` on edited documents.

Implementation work should use targeted regressions first. From `ic/`, all Cargo commands must use `taskset -c 0-5` and `-j6`; use `cargo check`, not a debug build. Security changes require both negative and intentional-private-network positive cases. Secret-envelope work requires migration fixtures from real pre-change rows and must not proceed without a rollback/read-compatibility design.

## Bottom line

There is useful material in IronClaw 1.0.0-rc.1, but almost all of it is in narrow failure-handling mechanisms, not the architecture cutover. The strongest additions to LunarWing's existing documentation are row-bound secret encryption, browser-send idempotency, reserved SSE error handling, routine hold/schedule correctness, compaction and stale-edit circuit breakers, durable extension cleanup, and crash/deployment validation. The Responses API is now mature enough upstream to reconsider, but it is a separate product investment rather than a safe cherry-pick.
