# Engine V2 Channel Parity Audit: XMPP, DarkIRC, and WeeChat

**Date:** 2026-07-18
**Branch:** `pre-release-testing`
**Commit:** `bad830c`
**Scope:** Engine V2 routing and delivery parity for XMPP, DarkIRC, and WeeChat, including conversation identity, controls, statuses, proactive delivery, attachments, security boundaries, and test coverage
**Method:** Static inspection of the Engine V2 router, agent dispatch path, WASM wrapper, exact symlinked channel sources, capabilities, and tests; targeted Rust tests; and review of earlier disposable-tenant DarkIRC operational evidence. This audit did not modify channel/runtime code or send a live protocol message.
**Implementation backlog:** [`ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md`](../plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md)

---

## Table of Contents

1. [Executive Verdict](#1-executive-verdict)
2. [Scope and Source Resolution](#2-scope-and-source-resolution)
3. [Shared Engine V2 Routing](#3-shared-engine-v2-routing)
4. [DarkIRC Assessment](#4-darkirc-assessment)
5. [WeeChat Assessment](#5-weechat-assessment)
6. [Cross-Cutting Findings](#6-cross-cutting-findings)
7. [Security Findings](#7-security-findings)
8. [Test Coverage Assessment](#8-test-coverage-assessment)
9. [Verification Evidence](#9-verification-evidence)
10. [Recommendations](#10-recommendations)
11. [Rollout Decision](#11-rollout-decision)

---

## 1. Executive Verdict

| Area | Verdict | Summary |
|------|---------|---------|
| **Shared Engine V2 routing** | **PASS** | Exact opt-in policy, terminal response routing, metadata preservation, scoped conversations, approval/auth matching, interrupts, hooks, errors, and compatibility history are wired correctly. |
| **XMPP baseline** | **PARTIAL** | XMPP has the strongest adapter implementation, including attachments and proactive delivery, but Engine V2 does not currently consume incoming attachments. Its real-WASM test does not prove a deployed XMPP bridge. |
| **DarkIRC** | **PARTIAL** | Correct for the current text-only DM milestone: reactive replies, proactive nick targeting, actionable statuses, pairing, metadata, and scoped controls are implemented. It lacks attachment support, generic status delivery, stable authenticated identity, and a real-WASM Engine V2 integration test. |
| **WeeChat** | **FAIL for full parity** | Reactive text replies and DM/group control scoping work, but proactive delivery is a silent no-op. The status path has a UTF-8 panic, group authentication privacy differs from XMPP, and the default open-DM policy is risky with owner-scope fallback. |

The core Engine V2 bridge is channel-neutral for final text responses. The adapters are not equally complete. DarkIRC is suitable for a constrained text-only DM rollout. WeeChat should not be declared Engine V2 parity-complete until its proactive and status defects are fixed and tested through a real component.

---

## 2. Scope and Source Resolution

The tracked channel source paths resolve to these implementations:

| Channel | Tracked path | Effective source |
|---------|--------------|------------------|
| DarkIRC | `ic/channels-src/darkirc` | `darkirc_channel_for_lunarwing/darkirc` |
| WeeChat | `ic/channels-src/weechat` | `lunarwing_weechat_wss/weechat_relay` |
| XMPP | `ic/channels-src/xmpp` | In-tree XMPP WASM channel |

The audit inspected the effective sources rather than assuming the contents of the link locations.

Primary shared files:

- `ic/src/bridge/router.rs`
- `ic/src/agent/agent_loop.rs`
- `ic/src/agent/thread_ops.rs`
- `ic/src/channels/channel.rs`
- `ic/src/channels/manager.rs`
- `ic/src/channels/wasm/setup.rs`
- `ic/src/channels/wasm/wrapper.rs`
- `ic/tests/engine_v2_channel_delivery.rs`

Primary adapter files:

- `darkirc_channel_for_lunarwing/darkirc/src/lib.rs`
- `darkirc_channel_for_lunarwing/darkirc/darkirc.capabilities.json`
- `lunarwing_weechat_wss/weechat_relay/src/lib.rs`
- `lunarwing_weechat_wss/weechat_relay/weechat.capabilities.json`
- `lunarwing_weechat_wss/weechat_relay/ws_adapter.py`
- `ic/channels-src/xmpp/src/lib.rs`
- `ic/channels-src/xmpp/xmpp.capabilities.json`

### Operational evidence incorporated

A disposable tenant, `darktest2`, was previously provisioned on Gentoo/OpenRC with DarkIRC enabled and no external workers. It was configured with:

```text
ENGINE_V2=true
ENGINE_V2_CHANNELS=darkirc
```

The tenant restarted successfully through `ic/scripts/lunarwing-mt-admin.sh`; automatic DarkIRC adapter-secret upload was successful and idempotent; authenticated adapter health and setup requests returned HTTP 200; and no post-refresh HTTP 401 errors were observed. No credential value is reproduced in this report.

This proves tenant configuration, service startup, secret seeding, and adapter authentication. It does not prove a real DarkIRC message traversing adapter -> WASM -> Engine V2 -> WASM -> adapter.

---

## 3. Shared Engine V2 Routing

### Routing policy: PASS

`ic/src/bridge/router.rs:39-82` defines the eligible non-gateway channels as:

```rust
["xmpp", "darkirc", "weechat"]
```

The policy has the intended behavior:

- `ENGINE_V2=false` disables Engine V2 for all channels.
- `gateway` uses Engine V2 whenever `ENGINE_V2=true`.
- XMPP, DarkIRC, and WeeChat require exact entries in `ENGINE_V2_CHANNELS`.
- Entries are trimmed and compared case-insensitively.
- Empty, unknown, and substring entries do not qualify.

Changing `ENGINE_V2_CHANNELS` requires a tenant restart. Channels not listed remain on the legacy path.

### Terminal delivery: PASS

Engine V2 returns terminal text to the existing outer agent handler. The handler applies the normal outbound hook, suppresses an empty no-reply sentinel, and calls `ChannelManager::respond()` once. WASM delivery then invokes `on_respond` with the original incoming metadata at `ic/src/channels/wasm/wrapper.rs:2772-2806`.

This preserves the adapter-specific target:

- XMPP JID or room
- DarkIRC nick
- WeeChat buffer/network/target tuple

The delivery matrix verifies that stream status precedes the terminal response and that one assistant history entry is written.

### Conversation and control scope: PASS

Engine conversations use `channel:conversation_scope` plus `user_id` (`ic/src/bridge/router.rs:84-95`). DarkIRC and WeeChat use non-UUID scope strings, so the router resolves their conversations by the full engine conversation key instead of attempting UUID parsing (`ic/src/bridge/router.rs:1392-1506`).

Dedicated unit tests verify:

- DarkIRC DM approval and authentication gates do not cross-match another DM scope.
- WeeChat DM and group approval/authentication gates do not cross-match.
- A DarkIRC interrupt stops only the active thread in that DM scope.
- A WeeChat interrupt stops only the active thread in the selected DM or group scope.

### Status and streaming policy: PASS at the shared boundary

Engine thread events are converted into `StatusUpdate` values. `ResponseDelta` becomes `StatusUpdate::StreamChunk`; the gateway renders it through SSE, while WASM channels deliberately ignore individual chunks to avoid one protocol message per token. The complete terminal response still uses `on_respond`.

Adapter-specific status rendering remains uneven and is covered below.

---

## 4. DarkIRC Assessment

### Working behavior

DarkIRC emits each accepted DM with:

- `user_id` and `user_name` set to the sender nick
- `thread_id` set to `darkirc:dm:<nick>`
- metadata containing `{ "nick": "<nick>" }`
- no attachments

See `darkirc_channel_for_lunarwing/darkirc/src/lib.rs:471-483`.

Reactive delivery parses the original metadata and sends to the nick (`src/lib.rs:331-337`). Proactive delivery accepts an explicit target nick and sends through the same path (`src/lib.rs:339-341`). Long replies are split at UTF-8-safe boundaries.

The DM policy defaults to `pairing`, combines configured allowlist entries with the shared pairing store, and performs case-insensitive nick checks. This is materially safer than WeeChat's current default.

Actionable statuses are supported for:

- approval needed
- authentication required
- authentication completed
- job started

DarkIRC's status truncation uses `floor_char_boundary`, with regression tests for multibyte text and emoji (`src/lib.rs:667-677`, tests at `src/lib.rs:904-979`).

### Gaps

1. **No attachment support.** Inbound messages always emit an empty attachment list. `on_respond` and `on_broadcast` ignore `AgentResponse.attachments`.
2. **No group conversation support.** The adapter is intentionally DM-only.
3. **Generic status is ignored.** Only four actionable status variants are rendered. `StatusType::Status`, including the Engine V2 external-gate waiting indication, is dropped (`src/lib.rs:343-381`).
4. **Nick identity is not a stable authenticated principal.** Conversation scope and pairing are nick-based. Nick reuse or network-specific case behavior can create continuity risks that do not exist with a stable XMPP bare JID.
5. **No real-WASM Engine V2 integration test.** Unit tests cover parsing, splitting, truncation, and serialization, but not an instantiated component communicating with a mock adapter through the Engine V2 agent loop.

### DarkIRC verdict

**PARTIAL: acceptable for controlled text-only DM rollout, not full XMPP parity.**

The current `darktest2` setting, `ENGINE_V2_CHANNELS=darkirc`, is consistent with a disposable or constrained rollout if pairing remains enforced and attachments/proactive mission guarantees are not assumed.

---

## 5. WeeChat Assessment

### Working behavior

WeeChat supports both DMs and IRC group buffers. It emits metadata containing:

- full buffer name
- network
- target
- sender nick
- DM/group flag

Real scopes are generated as:

```text
weechat:dm:<network>:<nick>
weechat:group:<network>:<target>
```

See `lunarwing_weechat_wss/weechat_relay/src/lib.rs:1309-1334`.

Reactive `on_respond` uses this metadata to send to the correct DM or group buffer and splits long IRC responses (`src/lib.rs:464-541`). The polling and long-poll paths share tag filtering that rejects self messages and prevents response mirror loops.

The shared router's dedicated tests confirm DM/group gate and interrupt isolation.

### Finding W-1: Proactive delivery is a silent no-op

**Severity: High**

`on_broadcast` returns success without sending anything:

```rust
fn on_broadcast(_user_id: String, _response: AgentResponse) -> Result<(), String> {
    Ok(())
}
```

Source: `lunarwing_weechat_wss/weechat_relay/src/lib.rs:280-283`.

This is silent data loss. The WASM wrapper receives `Ok(())`, so callers believe delivery succeeded. Affected callers include:

- Engine V2 mission notifications (`ic/src/bridge/router.rs:3008-3036`)
- the built-in message tool (`ic/src/tools/builtin/message.rs:333-365`)
- other proactive channel broadcasts

Reactive replies are unaffected because they use `on_respond`.

### Finding W-2: UTF-8 status truncation can trap

**Severity: Medium**

WeeChat truncates status messages using a byte slice:

```rust
let truncated = if message.len() > 400 {
    format!("{}...", &message[..397])
} else {
    message.to_string()
};
```

Source: `lunarwing_weechat_wss/weechat_relay/src/lib.rs:566-570`.

`String::len()` is a byte count, and byte 397 can fall inside a multibyte character. For example, 201 repetitions of `e` with an acute accent occupy 402 UTF-8 bytes and place byte 397 inside a code point. Slicing there panics and traps the WASM callback. The wrapper treats statuses as best-effort, so the daemon survives, but the user may never receive the auth/job status.

DarkIRC already contains the correct boundary-safe implementation and regression tests.

### Finding W-3: Authentication status can be posted to a group

**Severity: Medium**

WeeChat's `on_status` sends actionable status text to either a DM or group buffer according to metadata (`src/lib.rs:543-595`). XMPP explicitly returns without sending status when metadata identifies a group chat (`ic/channels-src/xmpp/src/lib.rs:231-234`).

An Engine V2 authentication status can include setup instructions and an OAuth URL. Posting it into an IRC group can expose an authorization URL or state value to every participant. WeeChat should match the XMPP privacy boundary unless group auth prompts are an explicit supported policy.

### Finding W-4: Default DM policy is open

**Severity: High when no owner actor is configured**

The source comment and setup prompt describe `pairing` as the default, but the implementation and capabilities use `open`:

- `default_dm_policy()` returns `"open"` at `src/lib.rs:211-213`.
- `weechat.capabilities.json:117` sets `"dm_policy": "open"`.
- `weechat_local_config.json.example:2` also sets `"dm_policy": "open"`.

When no `wasm_channel_owner_ids` entry exists, the WASM wrapper maps every sender to the instance owner's execution scope so the sender inherits the owner's workspace and secrets (`ic/src/channels/wasm/wrapper.rs:760-772`). The owner-ID map defaults empty.

The combined default is risky: an unpaired IRC DM can run under owner scope. Operators can override the policy through the adapter config, but the shipped default and documentation are inconsistent. This risk exists on both legacy and Engine V2 paths; Engine V2 opt-in does not create it, but parity review cannot treat the channel as safe by default while it remains.

### Finding W-5: No attachment support

Inbound WeeChat messages always emit an empty attachment list (`src/lib.rs:1327-1334`). Reactive and proactive callbacks do not implement attachment delivery. This is below XMPP adapter capability.

### WeeChat verdict

**FAIL for full parity; PARTIAL for reactive text-only conversations.**

DM/group ingestion, metadata routing, reactive final text, and scoped Engine V2 controls work. Proactive delivery, status safety/privacy, default DM access control, attachments, and real-component integration coverage do not meet the XMPP baseline.

---

## 6. Cross-Cutting Findings

### Finding C-1: Engine V2 bypasses attachment augmentation

**Severity: Medium; affects XMPP most visibly**

The agent branches into Engine V2 at `ic/src/agent/agent_loop.rs:1424-1486` before the legacy path augments content with attachments at `ic/src/agent/thread_ops.rs:438-455`.

Consequences:

- XMPP can decode and emit incoming attachments, but Engine V2 passes only the original text to the engine.
- Transcribed audio and extracted document text are not attached to the current Engine V2 prompt.
- Image data is not supplied as multimodal content.
- An image-only message can hit empty-input validation instead of the legacy `[attachment]` placeholder behavior at `ic/src/agent/thread_ops.rs:325-347`.

This means XMPP itself is not fully attachment-compatible with Engine V2 despite the adapter implementing inbound and outbound attachment conversion.

### Finding C-2: Generic external-gate status is lost on IRC adapters

**Severity: Low to Medium**

For an external Engine V2 gate, the router emits:

```text
Waiting for external confirmation (gate: ...) ...
```

as `StatusUpdate::Status` (`ic/src/bridge/router.rs:289-309`). XMPP renders ordinary `Status` messages in DMs. DarkIRC and WeeChat ignore the variant because their `on_status` matches only approval, auth, and job statuses.

Users can therefore see a turn stop progressing without receiving the waiting explanation.

### Finding C-3: Proactive owner routing remains under-tested

The WASM wrapper persists the last owner routing metadata only when it can identify the sender as the configured owner. If no channel-specific owner actor is configured, messages execute under owner scope but are not marked as owner-sender messages for broadcast-metadata persistence (`ic/src/channels/wasm/wrapper.rs:760-772`, `2594-2645`).

Direct broadcasts with an explicit protocol target can still work for adapters that implement `on_broadcast`. Owner-scoped mission notifications depend on previously stored routing metadata and need real per-channel tests.

---

## 7. Security Findings

### Finding S-1: Runtime config logging can expose credentials

**Severity: High when debug logging is enabled**

The setup path injects raw channel secrets into config for XMPP and WeeChat (`ic/src/channels/wasm/setup.rs:476-525`). `WasmChannel::update_config` then logs the complete merged JSON at debug level (`ic/src/channels/wasm/wrapper.rs:896-919`). WeeChat logs the full config again inside `on_start` (`lunarwing_weechat_wss/weechat_relay/src/lib.rs:285-290`).

This can expose:

- `relay_password` for WeeChat
- `xmpp_password` for XMPP through the shared wrapper log
- any other secret-bearing config field added later

The default multi-tenant log level is info, so this is not emitted in the normal default configuration. Enabling debug logging for troubleshooting activates the exposure. Config logs must redact known secret fields or be removed.

DarkIRC's adapter secret is safer in this respect because it is host-injected as an HTTP credential and is not passed into the WASM config.

### Finding S-2: IRC identity is weaker than XMPP identity

DarkIRC scopes and pairs by nick. WeeChat DM scope is also nick-based even though sender metadata may contain a hostmask. With owner-scope fallback, the hostmask is preserved as `sender_id` but is not part of the engine conversation key.

Nick changes, nick reuse, or network case-folding differences can split or transfer conversation continuity. This is a protocol identity limitation rather than an Engine V2 routing defect, but approval/auth controls depend on the same scope and should not assume nick ownership is cryptographically stable.

---

## 8. Test Coverage Assessment

### What the Engine V2 matrix proves

`ic/tests/engine_v2_channel_delivery.rs` verifies:

- exact opt-in route selection
- terminal response delivery and ordering
- original metadata preservation
- one compatibility-history assistant entry
- outbound hook modification and rejection
- one error response
- interrupt cancellation without a late terminal response
- approval resume without duplicate execution
- auth isolation and no credential-history persistence

### What it does not prove

The matrix constructs an in-process `TestChannel` and changes only its reported channel name (`ic/tests/support/test_channel.rs`, `ic/tests/engine_v2_channel_delivery.rs:561-624`). Cases named `darkirc`, `weechat`, and `xmpp` therefore do not instantiate those WASM components or exercise their HTTP adapters.

The existing real-XMPP wrapper test loads an actual XMPP WASM module when available and verifies runtime config survives into `on_respond` (`ic/src/channels/wasm/wrapper.rs:3644-3678`). It intentionally points at an unreachable bridge, so it still does not prove deployed XMPP protocol delivery.

There are no equivalent real-component wrapper tests for DarkIRC or WeeChat.

### Missing high-value tests

1. Instantiate the real DarkIRC WASM component with a mock loopback adapter and verify poll -> emitted message -> Engine V2 -> `on_respond` -> adapter send.
2. Instantiate the real WeeChat component with a mock relay/adapter and verify DM and group reactive responses.
3. Verify `on_broadcast` causes an observable send for each channel; a success return without a send must fail the test.
4. Send long multibyte auth/status text through each real component and assert no trap.
5. Verify auth URLs are not posted to WeeChat group buffers.
6. Exercise approval, auth, and interrupt through the real component metadata rather than manually constructed `IncomingMessage` fixtures.
7. Exercise an XMPP image-only and document/audio attachment through Engine V2.
8. Run one disposable-tenant live message test per adapter after local component tests pass.

---

## 9. Verification Evidence

All meaningful targeted tests passed. Cargo commands were CPU-limited to six cores; remaining long-running checks were executed in detached `tmux` sessions with logs under `/tmp`.

### Engine V2 delivery matrix

```bash
taskset -c 0-5 cargo test -j6 --features "libsql integration" \
  --test engine_v2_channel_delivery engine_v2_channel_delivery_matrix \
  -- --exact --nocapture --test-threads=6
```

Result: **1 passed, 0 failed**.

### DarkIRC host/router checks

```bash
taskset -c 0-5 cargo test -j6 --features "libsql integration" \
  --lib darkirc_ -- --nocapture --test-threads=6
```

Result: **3 passed, 0 failed**.

Covered adapter URL injection, DM gate isolation, and scoped active-thread interruption.

### WeeChat host/router checks

```bash
taskset -c 0-5 cargo test -j6 --features "libsql integration" \
  --lib weechat_ -- --nocapture --test-threads=6
```

Result: **3 passed, 0 failed**.

Covered relay-password injection, DM/group gate isolation, and scoped active-thread interruption.

### Routing policy

```bash
taskset -c 0-5 cargo test -j6 --features "libsql integration" \
  --lib engine_v2_channel_policy_is_gateway_default_and_exact_opt_in \
  -- --nocapture --test-threads=6
```

Result: **1 passed, 0 failed**.

### DarkIRC adapter units

```bash
taskset -c 0-5 cargo test -j6 \
  --manifest-path darkirc_channel_for_lunarwing/darkirc/Cargo.toml \
  -- --nocapture --test-threads=6
```

Result: **25 passed, 0 failed**.

### WeeChat adapter units

```bash
taskset -c 0-5 cargo test -j6 \
  --manifest-path lunarwing_weechat_wss/weechat_relay/Cargo.toml \
  -- --nocapture --test-threads=6
```

Result: **13 passed, 0 failed**.

The worktree was clean after verification and remained at commit `bad830c` before this documentation-only change.

---

## 10. Recommendations

### P0: Before declaring WeeChat parity

1. **Implement `on_broadcast`.** Route explicit targets to the correct WeeChat DM or buffer and return an error when no valid target can be resolved. Never return success without an observable send.
2. **Make status truncation UTF-8 safe.** Reuse the DarkIRC boundary-safe helper pattern and add multibyte/emoji regression tests.
3. **Suppress authentication status in group buffers.** Match XMPP unless an explicit opt-in policy is designed for group auth prompts.
4. **Resolve the DM default mismatch.** Prefer `pairing` or `allowlist`; changing `open` is a behavioral/security default change and should be documented for existing deployments.
5. **Remove or redact full config logging.** Cover both the shared wrapper and WeeChat `on_start`; include a regression test proving credential values never appear in captured logs.

### P1: Engine V2 and adapter completeness

6. **Move attachment augmentation before the Engine V2 branch or add an Engine V2 attachment input contract.** Preserve image-only validation behavior and multimodal content.
7. **Render generic waiting statuses on IRC adapters.** At minimum surface external-gate waiting state in a DM-safe form.
8. **Add real-WASM mock-adapter integration tests for DarkIRC and WeeChat.** Make these a prerequisite for broad rollout.
9. **Test proactive owner routing.** Verify mission notifications work after restart and do not depend on an unrepresentable owner actor ID.

### P2: Identity hardening

10. **Document IRC identity limitations.** Explain nick reuse, case folding, and pairing implications.
11. **Prefer stronger WeeChat sender scope when available.** Consider incorporating normalized network plus authenticated account/host identity into DM scope without breaking persisted conversations silently.

---

## 11. Rollout Decision

### DarkIRC

**Continue exact opt-in only for controlled text-only DM use.** Keep `ENGINE_V2_CHANNELS=darkirc` on the disposable `darktest2` tenant while pairing is enforced. Do not advertise attachment or full proactive-notification parity. A real message round trip remains the next live validation step.

### WeeChat

**Do not declare Engine V2 parity-complete.** Keep it off production `ENGINE_V2_CHANNELS` configurations where proactive notifications, safe authentication prompts, or default DM isolation are required. Reactive text-only testing may continue on a disposable tenant after the P0 defects are fixed.

### XMPP

**Retain as the adapter capability baseline, with an explicit Engine V2 attachment caveat.** The adapter supports attachments, but the current Engine V2 branch does not pass them into the engine prompt. Deployed protocol delivery still requires separate live validation.
