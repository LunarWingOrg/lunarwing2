---
plan name: ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS
plan description: Actionable remediation backlog for XMPP, DarkIRC, and WeeChat Engine V2 parity
plan status: in progress
source audit: docs/reviews/ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md
---

# Engine V2 Channel Parity Work Items

This document converts the findings in
[`ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md`](../reviews/ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md)
into implementation-ready work items.

The audit remains the evidence and rationale record. This document is the
execution backlog: each item states the problem, required implementation,
affected areas, acceptance criteria, tests, and dependencies.

No item in this document is implemented merely by creating this plan.

---

## Status Legend

- `[ ]` Not started
- `[-]` In progress
- `[x]` Completed and verified
- `[!]` Blocked or requires a decision

An item is complete only when its acceptance criteria and required tests pass.
Source changes without verification do not qualify as complete.

---

## Priority and Sequence

| ID | Priority | Work item | Primary area | Depends on |
|----|----------|-----------|--------------|------------|
| CHPAR-001 | P0 | Remove secret-bearing config logging | Shared WASM host, WeeChat | None |
| CHPAR-002 | P0 | Implement real WeeChat proactive delivery | WeeChat, shared target routing | None for explicit targets; CHPAR-008 for owner-scoped missions |
| CHPAR-003 | P0 | Make WeeChat status truncation UTF-8 safe | WeeChat | None |
| CHPAR-004 | P0 | Prevent auth status from leaking into WeeChat groups | WeeChat | None |
| CHPAR-005 | P0 | Change the WeeChat new-install DM default to pairing | WeeChat config and docs | Operator compatibility decision |
| CHPAR-006 | P1 | Pass attachments into Engine V2 | Agent, bridge, engine/LLM adapter | Input-contract decision |
| CHPAR-007 | P1 | Surface external-gate waiting status on IRC channels | Bridge, DarkIRC, WeeChat | None |
| CHPAR-008 | P1 | Define secure proactive owner-target routing | Shared WASM wrapper/config | None |
| CHPAR-009 | P1 | Add real DarkIRC WASM + Engine V2 integration tests | DarkIRC, integration tests | CHPAR-008 for owner broadcast cases |
| CHPAR-010 | P1 | Add real WeeChat WASM + Engine V2 integration tests | WeeChat, integration tests | CHPAR-002 through CHPAR-005 |
| CHPAR-011 | P2 | Document and harden IRC sender identity | DarkIRC, WeeChat, config/docs | Persistence compatibility decision |
| CHPAR-012 | P2 | Run disposable-tenant live protocol validation | Operations and rollout | CHPAR-001 through CHPAR-010 |

### Required implementation order

1. Complete CHPAR-001, CHPAR-003, and CHPAR-004 first because they are isolated
   security/correctness fixes.
2. Apply the approved CHPAR-005 new-install default. Keep CHPAR-008 deferred and
   do not infer an owner from arbitrary IRC traffic.
3. Implement CHPAR-002 explicit-target delivery with its real-component coverage
   in CHPAR-010. Do not claim owner-scoped mission delivery.
4. Implement shared Engine V2 completeness work in CHPAR-006 and CHPAR-007.
5. Complete real-component tests in CHPAR-009 and CHPAR-010.
6. Perform CHPAR-012 only after all applicable local gates pass.

---

## Current Status (2026-07-18)

Only **CHPAR-003** currently qualifies as completed and verified under this
plan's item-level acceptance criteria. Several other items have implementation
changes but still lack required verification; they must not be reported as
complete.

| ID | Status | Current finding / remaining gate |
|----|--------|----------------------------------|
| CHPAR-001 | [-] In progress | Raw config logging was removed from the shared wrapper and channel guests. Required tracing-capture tests proving sentinel XMPP and WeeChat passwords never appear are still missing. |
| CHPAR-002 | [-] In progress | Explicit `irc.<network>.<target>` WeeChat delivery, validation, chunking, DM fallback, and attachment rejection are implemented. Native unit tests pass. The real-WASM loopback test exists but has not executed locally because the required WASM target/artifact is unavailable. Owner-scoped mission routing is excluded. |
| CHPAR-003 | [x] Completed | UTF-8-safe byte-budget truncation is implemented and covered for ASCII, two-byte, three-byte, and four-byte input. The WeeChat adapter suite passes. |
| CHPAR-004 | [-] In progress | Group `AuthRequired` and `AuthCompleted` suppression is implemented. Required mock-relay assertions for DM delivery and zero group sends are missing. |
| CHPAR-005 | [-] In progress | New-install defaults, capabilities, example config, fail-closed policy handling, and docs use `pairing`. Config tests pass. Pairing approval and persisted-upgrade fixtures are missing. |
| CHPAR-006 | [!] Decision required | No implementation started. The Engine V2 multimodal input contract remains undecided. |
| CHPAR-007 | [ ] Not started | No implementation or tests. |
| CHPAR-008 | [!] Deferred | Numeric `wasm_channel_owner_ids` and existing 1.1.2 persistence/identity behavior are intentionally unchanged. Requires a separate migration and rollback design. |
| CHPAR-009 | [ ] Not started | No real DarkIRC WASM + Engine V2 fixture exists. |
| CHPAR-010 | [-] In progress | A real-WeeChat wrapper test and loopback `/api/input` fixture were added for explicit proactive group/DM delivery, fallback, auth, and invalid targets. The component could not be built/executed locally; all other required scenarios remain. |
| CHPAR-011 | [!] Decision required | No identity migration policy has been approved; no implementation started. |
| CHPAR-012 | [!] Blocked | Live protocol validation remains blocked by incomplete P0/P1 verification. |

### Verification recorded in this worktree

- WeeChat adapter unit suite: **28 passed, 0 failed**.
- Core message-tool unit suite: **25 passed, 0 failed**.
- Targeted core `cargo check` with `libsql integration`: passed.
- Core and WeeChat formatting checks: passed.
- Real WeeChat WASM execution: **not run**. The build attempt failed because
  this VM does not have the `wasm32-wasip1` standard library and does not have
  `rustup`; consequently no `weechat_relay_channel.wasm` artifact was produced.
- The real-WASM test fails in CI when the required artifact is absent, matching
  CHPAR-010's non-skip CI requirement. Locally it reports the missing artifact
  and returns without claiming protocol proof.

---

## P0: Security and Correctness Blockers

## CHPAR-001: Remove Secret-Bearing Config Logging

**Status:** [-] Implementation complete; captured-log regression pending

**Problem:** Raw channel secrets are injected into runtime config for XMPP and
WeeChat. The shared WASM wrapper logs the merged config at debug level, and the
WeeChat guest logs the full config again during `on_start`. Enabling debug logs
can expose `xmpp_password` or `relay_password`.

**Required implementation:**

1. Remove `config = %*config_guard` from `WasmChannel::update_config`, or replace
   it with a safe summary containing only non-sensitive key names and booleans.
2. Remove the full `config_json` value from WeeChat's `on_start` log.
3. Audit every WASM channel for full config logging after host-side secret
   injection.
4. Keep secret values out of log fields, formatted error text, panic text, and
   test output.
5. Preserve useful troubleshooting fields such as channel name, configured
   endpoint presence, and connection mode without printing credential values.

**Likely files:**

- `ic/src/channels/wasm/wrapper.rs`
- `ic/src/channels/wasm/setup.rs`
- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`
- other `ic/channels-src/*/src/lib.rs` files found by the audit

**Acceptance criteria:**

- [ ] A sentinel XMPP password never appears in captured debug logs.
- [ ] A sentinel WeeChat relay password never appears in captured debug logs.
- [x] Safe log output still identifies which channel received runtime updates.
- [x] Default info-level and opt-in debug-level startup both work in targeted tests.
- [ ] No secret value is added to snapshots or assertion failure messages.

**Required tests:**

- Add a tracing-capture regression around `update_config` with sentinel secrets.
- Add a WeeChat startup/logging regression or a source-level helper unit test for
  the sanitized summary.
- Run targeted WASM wrapper and setup tests.

**Current finding:** The host now logs only the channel name, key count, and key
names when runtime config changes. WeeChat and DarkIRC no longer log raw
`config_json` during startup. This closes the source-level leak, but the two
sentinel log-capture acceptance tests above are still required before completion.

---

## CHPAR-002: Implement Real WeeChat Proactive Delivery

**Status:** [-] Explicit-target delivery implemented; real-WASM proof pending

**Problem:** WeeChat `on_broadcast` returns `Ok(())` without sending. Mission
notifications and built-in message-tool calls can report success while silently
dropping the message.

**Target contract:** Use the full WeeChat buffer name as the canonical proactive
target:

```text
irc.<network>.<target>
```

Examples:

```text
irc.libera.#lunarwing
irc.libera.alice
```

A bare nick or bare channel name is ambiguous across multiple networks and must
not be accepted unless a network is supplied by an explicit, documented config
field.

**Required implementation:**

1. Make `on_broadcast` reject empty, malformed, or ambiguous targets.
2. Parse the full buffer target into network and IRC target.
3. Route group targets through `send_input` on the full buffer.
4. Route DM targets through `send_dm`, retaining its server-buffer fallback.
5. Split long proactive messages with the same chunking policy as `on_respond`.
6. Either implement attachment delivery or return an explicit unsupported error
   when `AgentResponse.attachments` is non-empty. Do not silently discard them.
7. Return an error if no chunk is delivered.
8. Update the built-in message-tool documentation/help to state the WeeChat
   target grammar.

**Likely files:**

- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`
- `ic/src/channels/wasm/wrapper.rs`
- `ic/src/channels/channel.rs`
- `ic/src/tools/builtin/message.rs`
- WeeChat operations/reference documentation

**Acceptance criteria:**

- [ ] Broadcast to `irc.<network>.#channel` produces an observable relay send.
- [ ] Broadcast to `irc.<network>.<nick>` produces an observable DM send.
- [ ] Invalid and ambiguous targets return `ChannelError::SendFailed` through the
  wrapper rather than success.
- [ ] A mission notification cannot report success when the relay received no
  send request.
- [ ] Long UTF-8 messages preserve order and data across chunks.
- [ ] Attachment behavior is explicit: delivered or rejected, never dropped.

**Required tests:**

- Unit tests for proactive target parsing and DM/group classification.
- Mock-relay tests asserting exact `/api/input` payloads.
- A real-WASM wrapper test invoking `Channel::broadcast`.
- An Engine V2 mission-notification test proving an observable send.

**Dependency:** Complete the target-source decision in CHPAR-008 before using
owner-scoped mission notifications.

**Compatibility note (2026-07-18):** The implementation accepts only explicit full WeeChat
buffer targets and does not change the owner-ID model. Owner-scoped mission notification routing
remains deferred with CHPAR-008. This removes the silent-success behavior for direct calls while
preserving persisted 1.1.2 tenant settings.

**Current finding:** Six new native tests cover group and DM parsing, dotted
recipients, malformed/ambiguous targets, empty content, and explicit attachment
rejection. A host-wrapper loopback test also asserts exact group and DM
`/api/input` requests, Basic authorization presence, DM server-buffer fallback,
and `ChannelError::SendFailed` for ambiguous targets. That test compiled but
returned early locally because the WeeChat WASM artifact could not be built.

---

## CHPAR-003: Make WeeChat Status Truncation UTF-8 Safe

**Status:** [x] Completed and verified

**Problem:** WeeChat slices status text at byte index 397. A multibyte character
crossing that offset traps the WASM callback.

**Required implementation:**

1. Extract a status truncation helper that operates on UTF-8 character
   boundaries.
2. Enforce the configured IRC byte budget, including the `[status] ` prefix and
   ellipsis.
3. Reuse the DarkIRC `floor_char_boundary` pattern or a shared equivalent.
4. Ensure short status text is unchanged.

**Likely file:**

- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`

**Acceptance criteria:**

- [x] ASCII text at and above the limit is handled correctly.
- [x] Two-byte, three-byte, and four-byte UTF-8 text never panics.
- [x] Output plus prefix stays within the configured byte budget.
- [x] The retained text is a valid prefix of the original followed by `...` when
  truncated.

**Required tests:**

- Port the DarkIRC multibyte and emoji status regression cases.
- Add a test whose byte 397 lies inside a multibyte character.
- Run the WeeChat adapter unit suite.

**Verification:** The helper budgets the complete `[status] ` prefix and
ellipsis, uses `floor_char_boundary`, and passes ASCII, two-byte, CJK
three-byte, and emoji four-byte regressions as part of the 28-test WeeChat
adapter suite.

---

## CHPAR-004: Prevent Auth Status from Leaking into WeeChat Groups

**Status:** [-] Implementation complete; mock-relay privacy verification pending

**Problem:** WeeChat sends `AuthRequired` and `AuthCompleted` status messages to
group buffers. These messages can contain setup instructions and OAuth URLs.
XMPP suppresses status delivery in group chats.

**Required implementation:**

1. Suppress `AuthRequired` and `AuthCompleted` when `metadata.is_dm == false`.
2. Preserve DM auth status delivery.
3. Decide separately whether approval prompts are allowed in groups. The shared
   wrapper currently sends approval prompts through `on_respond`, so changing
   only `on_status` does not alter approval behavior.
4. Log a safe debug event that a group auth status was suppressed; do not log the
   auth URL.

**Likely files:**

- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`
- optionally `ic/src/channels/wasm/wrapper.rs` if a shared policy is adopted

**Acceptance criteria:**

- [ ] DM auth-required and auth-completed messages are delivered.
- [ ] Group auth-required and auth-completed messages produce no relay send.
- [ ] OAuth URLs and state values are absent from group traffic and logs.
- [ ] Job-started behavior remains unchanged unless separately specified.

**Required tests:**

- DM/group metadata tests for both auth status variants.
- A mock-relay assertion that no group request was emitted.

**Current finding:** The source now returns before relay access for
`AuthRequired` and `AuthCompleted` when `metadata.is_dm == false`, and logs only
the status type and buffer name. DM behavior and `JobStarted` remain on the
existing path. No real-component or mock-relay test has yet demonstrated DM
delivery and zero group requests, so this item is not complete.

---

## CHPAR-005: Change the WeeChat New-Install DM Default to Pairing

**Status:** [-] Implementation complete; pairing and upgrade fixtures pending

**Problem:** Source comments and setup text describe `pairing` as the default,
but the implementation, capabilities config, and example adapter config use
`open`. With no configured owner actor, accepted senders execute under owner
scope and inherit owner workspace/secrets.

**Decision:** New installs should default to `pairing`. Existing persisted
operator choices must remain unchanged. An existing explicit `open` value must
not be silently rewritten during upgrade.

**Required implementation:**

1. Change `default_dm_policy()` to `pairing`.
2. Change the capabilities default to `pairing`.
3. Change the local adapter config example to `pairing`.
4. Ensure first-run setup with no stored value resolves to `pairing`.
5. Preserve an existing stored or adapter-provided `open`, `allowlist`, or
   `pairing` value.
6. Add release/operations documentation describing the new-install default and
   how operators can intentionally enable open DMs.
7. Validate unknown policy strings and fail closed rather than treating them as
   open.

**Likely files:**

- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`
- `lunarwing_weechat_wss/weechat_relay/weechat.capabilities.json`
- `lunarwing_weechat_wss/weechat_relay/weechat_local_config.json.example`
- `docs/ops/WEECHAT-SERVICES.md`
- `docs/architecture/WEECHAT-CHANNEL-ARCHITECTURE.md`

**Acceptance criteria:**

- [ ] A fresh setup with no policy accepts no unpaired DM into the agent loop.
- [ ] The sender receives pairing instructions once per new request.
- [ ] Pairing approval allows subsequent messages.
- [ ] An existing explicit `open` deployment remains open after upgrade.
- [ ] An invalid policy value fails closed and emits an actionable warning.

**Required tests:**

- Config-default and config-precedence tests.
- Pairing request/approval integration coverage.
- Upgrade fixture proving explicit persisted values are preserved.

**Current finding:** `default_dm_policy()`, the capabilities config, and the
example adapter config now use `pairing`; unknown policy strings reject the
sender with a warning. Three tests verify those shipped defaults. Existing
explicit values still flow through the existing precedence paths, but an actual
persisted `open` upgrade fixture and end-to-end pairing approval test have not
been added.

---

## P1: Engine V2 Completeness and Integration Proof

## CHPAR-006: Pass Attachments into Engine V2

**Status:** [!] Not started; input-contract design required

**Problem:** Engine V2 routing occurs before legacy attachment augmentation.
XMPP can emit attachments, but Engine V2 receives only original text. Image-only
messages can fail empty-input validation, and extracted audio/document content is
not attached to the current engine prompt.

**Required input behavior:**

1. Submission parsing must continue to inspect the original message text so an
   approval, interrupt, or auth token cannot be changed by attachment metadata.
2. Only ordinary `Submission::UserInput` should receive attachment augmentation.
3. Document and audio extracted text must be included in the Engine V2 user
   message using the existing sanitized `<attachments>` representation.
4. Image-only input must use a non-empty validation placeholder and must reach a
   provider-capable multimodal path.
5. Binary data must remain bounded by existing host and channel limits.
6. Credentials or local storage paths in attachment metadata must not be exposed
   to the model unless already allowed by the legacy contract.
7. Compatibility history should persist the same effective user text that the
   engine received, without duplicating binary data.

**Design options:**

- **Option A:** Extend `handle_with_engine` to accept an Engine input struct with
  augmented text and image parts.
- **Option B:** Extend engine `ThreadMessage` with typed multimodal content and
  map it in `LlmBridgeAdapter`.

Option B is the complete design. Option A can deliver extracted text first but
must not be called full attachment parity until image content is also supported.

**Likely files:**

- `ic/src/agent/agent_loop.rs`
- `ic/src/agent/attachments.rs`
- `ic/src/bridge/router.rs`
- `ic/src/bridge/llm_adapter.rs`
- `ic/crates/lunarwing_engine/src/types/*`
- LLM request/message conversion code

**Acceptance criteria:**

- [ ] XMPP image-only input is not rejected as empty.
- [ ] Extracted document text appears exactly once in the Engine V2 prompt.
- [ ] Audio transcription appears exactly once in the Engine V2 prompt.
- [ ] Image bytes or provider-native image content reach a multimodal-capable
  provider.
- [ ] Approval/auth/control submissions retain current parsing and secrecy.
- [ ] Attachment limits and leak scans remain enforced.
- [ ] V1 compatibility history contains no duplicated binary payload.

**Required tests:**

- Engine V2 image-only XMPP integration test.
- Document and audio extracted-text tests.
- Mixed text plus multiple attachments test.
- Approval/auth input with an attachment does not enter normal model history.
- Provider adapter assertion for image content parts.

---

## CHPAR-007: Surface External-Gate Waiting Status on IRC Channels

**Status:** [ ] Not started

**Problem:** The bridge emits external-gate waiting state as generic
`StatusUpdate::Status`. DarkIRC and WeeChat ignore all generic status messages,
so the user can see a turn stop without an explanation.

**Required implementation:**

1. Surface only known actionable waiting statuses, not every generic status.
2. Avoid forwarding reasoning narratives, noisy progress, or web-only status
   content to IRC.
3. Prefer adding a dedicated typed status if a WIT version change is acceptable.
4. If no WIT change is made, centralize a strict allowlist/classifier for the
   external-waiting message rather than matching arbitrary substrings in each
   adapter.
5. Apply the same UTF-8-safe byte budget as other IRC status messages.

**Likely files:**

- `ic/src/bridge/router.rs`
- `ic/wit/channel.wit` if a typed variant is added
- `ic/src/channels/wasm/wrapper.rs`
- `darkirc_channel_for_lunarwing/darkirc/src/lib.rs`
- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`

**Acceptance criteria:**

- [ ] External-gate pause produces one user-visible waiting message in a DM.
- [ ] Reasoning updates and stream chunks do not become IRC messages.
- [ ] Group privacy policy is explicit and tested.
- [ ] No duplicate terminal or waiting message is sent.

**Required tests:**

- Status conversion/classifier unit tests.
- Real-component DM delivery tests for DarkIRC and WeeChat.
- A negative test for reasoning and unknown generic status values.

---

## CHPAR-008: Define Secure Proactive Owner-Target Routing

**Status:** [!] Deferred for 1.1.2 owner-model compatibility

**Problem:** Owner-scoped broadcasts rely on stored last-routing metadata. That
metadata is updated only when the wrapper can identify the configured owner
actor. The existing owner actor map stores numeric IDs and is normally absent
for nick/JID-based channels. Treating every sender as the owner would allow a
guest to overwrite the proactive target.

**Required design properties:**

1. Do not infer owner identity from the most recent arbitrary sender.
2. Support string actor identifiers for XMPP JIDs and IRC nicks, or add an
   explicit per-channel default proactive target independent of actor identity.
3. Persist the chosen target under owner scope and restore it after restart.
4. Validate target syntax per channel before persistence.
5. Prevent a guest sender from replacing the owner's target.
6. Provide an operator-visible error when no safe target is configured.

**Compatibility decision (2026-07-18):**

- Keep `wasm_channel_owner_ids` numeric and preserve its existing DB keys, config shape,
  deserialization, and activation behavior.
- Do not change message scope, conversation identity, or owner fallback as part of proactive
  WeeChat delivery.
- Support explicit network-qualified WeeChat targets independently in CHPAR-002.
- Revisit automatic owner-scoped targets only with a dedicated migration and rollback plan.

**Previously recommended design (not approved for implementation):**

- Generalize channel owner actor IDs from numeric-only values to a string-backed
  representation while preserving existing numeric config deserialization.
- Add an explicit `default_proactive_target` per WASM channel for deployments
  where the owner cannot be identified reliably from incoming protocol traffic.
- For WeeChat, store the canonical full buffer name, not a bare target.
- For DarkIRC, store an explicit nick only after operator configuration or a
  verified owner message.

**Likely files:**

- `ic/src/config/channels.rs`
- `ic/src/settings.rs`
- `ic/src/channels/wasm/setup.rs`
- `ic/src/channels/wasm/wrapper.rs`
- `ic/src/extensions/manager.rs`
- channel setup capabilities and operations docs

**Acceptance criteria:**

- [ ] String owner actor IDs round-trip through config and DB settings.
- [ ] Existing numeric owner IDs continue to deserialize and behave correctly.
- [ ] Owner target survives process restart.
- [ ] Guest traffic cannot alter the persisted owner target.
- [ ] Missing target produces an actionable error, not silent delivery loss.
- [ ] WeeChat stores a full network-qualified buffer target.

**Required tests:**

- Config and DB round-trip tests for numeric and string owner IDs.
- Owner/guest metadata update isolation tests.
- Restart restoration test.
- Per-channel target validation tests.

---

## CHPAR-009: Add Real DarkIRC WASM + Engine V2 Integration Tests

**Status:** [ ] Not started

**Problem:** Current DarkIRC unit tests do not instantiate the real WASM
component or prove adapter communication through Engine V2.

**Required fixture:** A loopback mock adapter implementing:

- `GET /health`
- `GET /poll`
- `POST /ack`
- `POST /send`
- optional Bearer validation with a sentinel secret

**Required scenarios:**

1. Poll one paired DM and assert the exact `IncomingMessage` user, sender,
   conversation scope, content, and metadata.
2. Route that message through Engine V2 and assert one terminal `/send` to the
   original nick.
3. Assert the poll batch is acknowledged exactly once after a successful poll
   callback, and that malformed or failed poll responses are not acknowledged.
4. Verify approval prompt and approval response stay in the originating DM.
5. Verify auth status and credential response stay in the originating DM and the
   credential is absent from history/logs.
6. Verify interrupt cancels only the selected DM thread and sends no late final.
7. Verify explicit proactive target delivery.
8. Verify incorrect/missing adapter auth fails without leaking the secret.

**Likely files:**

- `ic/tests/engine_v2_darkirc_wasm.rs` or a channel-neutral real-WASM target
- `ic/tests/support/*`
- `ic/src/channels/wasm/wrapper.rs` test helpers
- DarkIRC component build/fixture scripts

**Acceptance criteria:**

- [ ] Tests load an actual built DarkIRC WASM component.
- [ ] HTTP assertions observe the adapter requests, not just `Channel` calls.
- [ ] The test fails if `on_respond` or `on_broadcast` becomes a no-op.
- [ ] No live DarkIRC network or tenant is required.
- [ ] CI fails when the required component is absent rather than silently
  skipping parity coverage.

---

## CHPAR-010: Add Real WeeChat WASM + Engine V2 Integration Tests

**Status:** [-] Real-WASM proactive fixture added; local execution blocked

**Problem:** Current WeeChat tests cover parsing and host/router scope but do not
instantiate the real component or prove relay requests.

**Required fixture:** A loopback mock relay/adapter implementing the endpoints
used by startup, long-poll, policy refresh, and send, including:

- `GET /api/version`
- `GET /api/buffers`
- `GET /api/buffers/<name>/lines`
- `GET /api/config`
- `GET /api/health`
- `GET /api/wait`
- `POST /api/input`
- Basic authorization checks with a sentinel password

**Required scenarios:**

1. Ingest and respond to a DM.
2. Ingest and respond to a group message.
3. Verify DM and group conversation scopes remain distinct.
4. Verify self-message tags cannot produce a response loop.
5. Verify approval/auth/interrupt controls remain in scope.
6. Verify group auth status is suppressed.
7. Verify long multibyte status cannot trap.
8. Verify proactive group and DM sends are observable.
9. Verify invalid proactive targets and relay failures return errors.

**Likely files:**

- `ic/tests/engine_v2_weechat_wasm.rs` or a channel-neutral real-WASM target
- `ic/tests/support/*`
- `ic/src/channels/wasm/wrapper.rs` test helpers
- WeeChat component build/fixture scripts

**Acceptance criteria:**

- [ ] Tests load an actual built WeeChat WASM component.
- [ ] Tests assert exact relay HTTP requests and authorization behavior.
- [ ] The test fails against the current no-op `on_broadcast` implementation.
- [ ] No installed WeeChat process or external IRC network is required.
- [ ] CI fails when the required component is absent rather than silently
  skipping parity coverage.

**Current finding:** A host-wrapper test now starts a loopback relay fixture and
is written to load the real WeeChat component, initialize it, broadcast to a
group, exercise DM server-buffer fallback, verify exact `/api/input` payloads
and Basic authorization, and reject an ambiguous target. CI panics if the
component artifact is absent. Local execution remains unproven because this VM
could not build the component (`wasm32-wasip1` standard library missing and no
`rustup`). Ingestion, reactive replies, scopes, controls, group-auth
suppression, and multibyte status scenarios remain to be implemented in this
fixture.

---

## P2: Identity and Live Rollout

## CHPAR-011: Document and Harden IRC Sender Identity

**Status:** [!] Deferred pending persistence compatibility decision

**Problem:** DarkIRC pairing and DM scope are nick-based. WeeChat preserves a
hostmask as `sender_id`, but DM conversation scope remains nick-based and can be
mapped to owner scope. Nick reuse, case-folding, and network identity changes can
split or transfer conversation continuity.

**Required implementation:**

1. Document current identity semantics and threat boundaries for both adapters.
2. Normalize IRC nick comparison using the network's case-mapping rules where
   available; do not assume ASCII lowercase is universally correct.
3. Prefer authenticated account identity from WeeChat tags when available.
4. Include network identity in every WeeChat DM principal and scope.
5. Define a migration policy before changing existing persisted conversation
   keys. Do not silently orphan or merge conversations.
6. Keep pairing approvals bound to the same normalized identity used for
   conversation scope.

**Likely files:**

- `darkirc_channel_for_lunarwing/darkirc/src/lib.rs`
- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`
- pairing store integration
- `docs/architecture/WEECHAT-CHANNEL-ARCHITECTURE.md`
- DarkIRC operations/reference documentation

**Acceptance criteria:**

- [ ] Identity format is documented with examples.
- [ ] Same nick on two WeeChat networks cannot share a DM conversation.
- [ ] Authenticated account identity is preferred when present.
- [ ] Case-variant behavior is deterministic and tested.
- [ ] Existing conversation compatibility has an explicit migration or retention
  policy.

---

## CHPAR-012: Run Disposable-Tenant Live Protocol Validation

**Status:** [!] Blocked until applicable P0/P1 items pass

**Problem:** Local tests do not prove deployed DarkIRC, WeeChat, or XMPP protocol
bridges. Prior `darktest2` evidence proves startup/authentication but not a real
chat round trip.

**Operational constraints:**

1. Use `ic/scripts/lunarwing-mt-admin.sh` for all tenant lifecycle operations.
2. Support and record the actual init system; never use bare system-bus
   `systemctl` against tenant units.
3. Keep all external workers disabled.
4. Never print adapter, gateway, relay, XMPP, or secrets-store credentials.
5. Use disposable tenants or channels and preserve unrelated tenants.
6. Use detached `tmux` for long Cargo/release operations with
   `taskset -c 0-5` and `-j6`.

**DarkIRC live scenarios:**

- Send one paired real DM through DarkIRC.
- Observe one Engine V2 thread in the expected DM scope.
- Receive one terminal response at the original nick.
- Exercise one scoped interrupt.
- Confirm no 401/auth errors and no duplicate response.

**WeeChat live scenarios:**

- Send one real DM and one real group message.
- Verify distinct Engine V2 scopes.
- Verify reactive responses.
- Verify proactive DM/group delivery after CHPAR-002 and CHPAR-008.
- Verify group auth suppression with a safe fixture credential request.

**XMPP live scenarios:**

- Send one normal chat message and verify one terminal response.
- Send image-only and document/audio attachment cases after CHPAR-006.
- Verify DM/group status privacy remains intact.

**Acceptance criteria:**

- [ ] Every requested message has exactly one observable terminal response.
- [ ] Approval, auth, and interrupt remain conversation-scoped.
- [ ] Proactive delivery reports success only when the protocol receives it.
- [ ] No secret appears in service logs or captured evidence.
- [ ] No external worker is created or started.
- [ ] Disposable resources are removed and port registry state is clean.
- [ ] A dated result is added to the source audit or a linked validation report.

---

## Global Definition of Done

Every code work item must satisfy the following before being marked complete:

- [ ] The narrowest unit and integration tests pass.
- [ ] Real-WASM coverage exists for behavior implemented inside a channel guest.
- [ ] `taskset -c 0-5 cargo fmt --all -- --check` passes from `ic/` when core
  Rust files are changed.
- [ ] A targeted `taskset -c 0-5 cargo check -j6` passes for affected core
  features. Do not use a full debug build.
- [ ] Long-running Cargo commands run in detached `tmux` with persistent logs.
- [ ] Secrets are absent from command arguments, logs, fixtures, snapshots, and
  user-facing evidence.
- [ ] Behavior changes update active operations/architecture documentation.
- [ ] Any WIT change rebuilds all affected channel components and verifies host
  compatibility.
- [ ] PostgreSQL remains the primary verified backend; existing libSQL coverage
  is not regressed.
- [ ] Multi-tenant service changes remain init-agnostic for systemd user managers
  and OpenRC.
- [ ] The final diff contains no unrelated generated artifacts or target output.

---

## Suggested Verification Commands

Run Cargo commands from `ic/` with repository resource limits. Use detached
`tmux` for commands expected to run longer than a few minutes.

```bash
taskset -c 0-5 cargo test -j6 --features "libsql integration" \
  --test engine_v2_channel_delivery -- --test-threads=6

taskset -c 0-5 cargo test -j6 --features "libsql integration" \
  --lib darkirc_ -- --test-threads=6

taskset -c 0-5 cargo test -j6 --features "libsql integration" \
  --lib weechat_ -- --test-threads=6

taskset -c 0-5 cargo check -j6 --all-features
taskset -c 0-5 cargo fmt --all -- --check
```

Run adapter unit suites from the repository root or by absolute manifest path:

```bash
taskset -c 0-5 cargo test -j6 \
  --manifest-path darkirc_channel_for_lunarwing/darkirc/Cargo.toml \
  -- --test-threads=6

taskset -c 0-5 cargo test -j6 \
  --manifest-path lunarwing_weechat_wss/weechat_relay/Cargo.toml \
  -- --test-threads=6
```

Do not mark protocol parity complete from the named `TestChannel` matrix alone.
