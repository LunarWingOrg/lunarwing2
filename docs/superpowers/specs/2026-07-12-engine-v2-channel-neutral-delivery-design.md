# Engine V2 Channel-Neutral Delivery Design

## Status

Approved direction, awaiting review of this written specification.

This design extends `docs/proposals/ENGINE_LLM_STREAMING.md`. Phase 0 remains
unchanged. The Phase 2 and Phase 3 delivery work must follow this design so
Engine V2 is not coupled to the web gateway or to a specific LLM vendor.

## Problem

Engine V2 currently works only for the web gateway. The routing gate in
`ic/src/agent/agent_loop.rs` accepts gateway messages and suppresses the normal
channel response because `await_thread_outcome` broadcasts the terminal reply
directly through gateway SSE.

That arrangement has two consequences:

1. XMPP, DarkIRC, and WeeChat can run the engine but cannot receive its final
   reply. Their outbound path is `Channel::respond()` and, for WASM channels,
   the `on_respond` callback.
2. The engine bridge treats gateway SSE as the authoritative delivery path,
   even though the gateway is intended to be one channel implementation among
   several.

The existing channel contracts already provide the correct boundary:

- `Channel::respond()` delivers the final response.
- `Channel::send_status()` delivers best-effort progress events.
- `GatewayChannel::respond()` maps a final response to `AppEvent::Response`.
- `GatewayChannel::send_status(StatusUpdate::StreamChunk)` maps a delta to
  `AppEvent::StreamChunk`.
- WASM channels intentionally ignore `StatusUpdate::StreamChunk`, while their
  `respond()` implementation invokes `on_respond` with the original message
  metadata for channel-specific routing.

## Goals

- Deliver every Engine V2 terminal reply exactly once through the originating
  channel's normal `respond()` path.
- Allow the gateway to render provider deltas through SSE without making SSE an
  engine dependency.
- Allow XMPP, DarkIRC, and WeeChat to opt into Engine V2 and receive complete
  replies through their existing WASM `on_respond` implementations.
- Preserve channel routing metadata, owner scope, conversation scope, thread
  identity, approval handling, authentication handling, hooks, and response
  suppression semantics.
- Keep provider-specific streaming protocols inside provider adapters.
- Preserve the current gateway-only default until each channel is explicitly
  enabled and tested.

## Non-Goals

- Emitting every provider token as a separate XMPP, DarkIRC, or WeeChat message.
- Adding message-edit support to channel protocols in the first milestone.
- Changing the Engine V2 approval or authentication model.
- Moving SSE types into the engine crate.
- Enabling Engine V2 for every channel merely because `ENGINE_V2=true`.
- Reworking the legacy agent loop.

## Design Decision

Terminal responses use the existing channel response path. Incremental events
use the existing best-effort status path.

```text
IncomingMessage
    -> Agent submission parsing and Engine V2 routing policy
    -> Engine V2 execution
       -> Thread progress/delta events
          -> ChannelManager::send_status
             -> gateway: SSE AppEvent
             -> WASM: supported statuses; StreamChunk ignored initially
       -> terminal reply or no-reply outcome
    -> existing outbound hook
    -> ChannelManager::respond
       -> gateway: terminal SSE Response
       -> WASM: on_respond using original routing metadata
```

The engine and provider layers produce typed domain events. They do not import
or construct `AppEvent`, `SseEvent`, WIT channel types, XMPP stanzas, IRC
messages, or WeeChat relay messages.

## Terminal Delivery

`handle_with_engine()` continues returning `Result<Option<String>, Error>`.
The agent loop maps an engine text result to `Some(text)` for the existing outer
response handler. An engine no-reply result maps to `Some(String::new())`, which
is the existing response-suppression signal. It must not return `None` to the
outer handler because that value currently means the agent received a shutdown
command.

`await_thread_outcome()` remains responsible for joining the thread, recording
the outcome, persisting the v1-compatible history entry, and producing the
terminal text. It no longer broadcasts `AppEvent::Response` directly.

The existing outer handler remains responsible for:

- running the `BeforeOutbound` hook;
- suppressing empty responses;
- calling `ChannelManager::respond()` once;
- converting an engine error into the existing user-visible error response.

This makes gateway and WASM terminal delivery use the same policy and prevents
double sends.

## Progress And Streaming Delivery

Engine thread events are translated into channel-neutral `StatusUpdate` values
at the bridge boundary. A future `EventKind::ResponseDelta` is translated to
`StatusUpdate::StreamChunk`.

The gateway already converts this status into `AppEvent::StreamChunk`, so SSE
remains incremental. WASM channels continue treating it as a no-op. This is an
intentional transport policy: sending one chat message per provider chunk would
be noisy, rate-limit prone, and difficult to reconcile after an interruption.

The bridge should stop separately translating the same thread event into direct
gateway `AppEvent` values once equivalent `StatusUpdate` mappings exist. During
migration, tests must ensure that a gateway event is emitted once, not once via
`send_status()` and again via direct SSE broadcast.

Visible incremental output for a non-gateway channel is a later channel feature.
It can add batching, message editing, or protocol-native typing behavior without
changing the engine/provider contracts.

## Routing Policy And Rollout

Engine V2 channel selection must be owned by the bridge/routing module rather
than hardcoded inline in the agent loop.

The rollout preserves the current behavior:

- `ENGINE_V2=false`: all channels use the legacy path.
- `ENGINE_V2=true` with no channel configuration: gateway uses Engine V2;
  other channels remain on the legacy path.
- `ENGINE_V2_CHANNELS` is a comma-separated allowlist. When set, it enables the
  named channels in addition to the gateway, beginning with `xmpp`, `darkirc`,
  and `weechat` after their tests pass.

The routing helper trims whitespace, compares lowercase exact channel names,
and ignores empty entries. It must not use substring matching. An unset or
empty `ENGINE_V2_CHANNELS` preserves the gateway-only default. Disabling
`ENGINE_V2` disables the engine for every channel regardless of the allowlist.

This staged policy provides rollback by removing a channel from the allowlist
without disabling Engine V2 for the gateway.

## Conversation And Reply Scope

The existing engine conversation key remains scoped by channel and conversation:

```text
channel_key = "<channel>:<conversation_scope>"
conversation = (channel_key, user_id)
```

The original `IncomingMessage` remains available until terminal delivery. This
is required because WASM `respond()` uses the original metadata to recover the
XMPP JID/room, DarkIRC target, or WeeChat target. The bridge must not reconstruct
an `IncomingMessage` from engine state.

For gateway statuses, the bridge supplies the engine thread ID in delivery
metadata so streamed chunks and tool statuses are associated with the current
frontend thread. Channel-specific metadata is cloned and extended, not replaced.

## Approval, Authentication, And Control Submissions

Submission parsing remains before Engine V2 routing. Plain user input, approval
responses, authentication tokens, interrupts, and other control submissions
must remain distinguishable.

When a channel is enabled for Engine V2:

- `ApprovalResponse` and `ExecApproval` must resolve Engine V2 pending gates
  when a matching engine gate exists.
- A simple `yes`, `no`, or `always` reply from a WASM channel must use the
  original conversation scope to avoid resolving another room's gate.
- Authentication input must continue to bypass normal LLM history and secret
  logging.
- Interrupt submissions must target the scoped Engine V2 thread.
- If no matching Engine V2 gate exists, existing legacy behavior remains
  available during the parallel rollout.

These paths require explicit integration tests before a WASM channel is added
to the Engine V2 allowlist. Final text delivery alone is not sufficient to call
a channel compatible.

## Provider Independence

Provider adapters own wire protocols such as OpenAI-compatible SSE, Responses
events, tool-call delta formats, terminal markers, and usage schemas. The
engine consumes only `LlmStreamChunk`.

The IronClaw comparison supports the channel-neutral runner boundary, but its
implementation is not a template to copy wholesale:

- its NearAI provider has native text streaming;
- its Rig adapter currently falls back to blocking completion;
- most decorators also fall back to blocking completion and therefore suppress
  native deltas;
- its retry decorator correctly avoids retrying after visible text is emitted.

LunarWing must test the configured provider/decorator chain end to end so a
provider-specific fallback cannot silently masquerade as native streaming.

## Error Handling

- Failure before terminal delivery returns through the existing agent error
  path and produces at most one channel response.
- A failure after visible gateway deltas must not retry or fail over in a way
  that duplicates already-visible text.
- A terminal response must not be emitted after a successful interrupt.
- Failure to send a best-effort status does not fail the engine turn.
- Failure to send the terminal response is logged by the existing outbound
  handler and is not retried blindly, because channel sends may be
  non-idempotent.
- Direct SSE and normal channel delivery must never both finalize the same turn.

## Testing Strategy

### Routing tests

- Gateway remains the only Engine V2 channel by default.
- Explicitly enabled `xmpp`, `darkirc`, and `weechat` messages enter Engine V2.
- Disabled and unknown channels remain on the legacy path.
- Commands and unrelated control submissions are not misrouted as user input.

### Delivery tests

- A completed Engine V2 gateway turn emits one terminal `AppEvent::Response`.
- A completed Engine V2 WASM turn invokes `on_respond` once with the original
  routing metadata.
- `BeforeOutbound` modification and suppression apply equally to Engine V2.
- Engine errors produce one error response.
- Persisted assistant history is written once.

### Streaming tests

- `ResponseDelta` becomes `StatusUpdate::StreamChunk`.
- Gateway converts the status to one SSE `stream_chunk` event.
- WASM channels do not invoke `on_respond` for individual chunks.
- The final WASM response still arrives after ignored chunks.
- No duplicate gateway status or terminal events are emitted during migration.

### Gate and scope tests

- XMPP room, DarkIRC target, and WeeChat conversation scopes do not collide.
- Approval replies resolve only the pending gate for the originating scope.
- Authentication tokens do not enter normal chat history.
- Interrupt targets the active Engine V2 thread for that scope.

## Implementation Sequence

1. Complete native provider streaming and decorator propagation as Phase 1 of
   the existing streaming proposal.
2. Add a tested Engine V2 channel routing policy with gateway-only defaults.
3. Make terminal Engine V2 results return through the normal outbound handler.
4. Remove direct terminal SSE delivery from `await_thread_outcome()`.
5. Route engine progress through `StatusUpdate`, preserving gateway thread
   metadata and eliminating duplicate direct SSE mappings.
6. Add Engine V2 approval/auth/interrupt routing tests for non-gateway scopes.
7. Enable and test final-response Engine V2 delivery for XMPP, DarkIRC, and
   WeeChat one channel at a time.
8. Consider channel-specific batched or editable streaming only after the
   final-response path is stable.

## Compatibility And Rollback

The default remains gateway-only. Phase 0 trait fallbacks remain blocking and
behavior-compatible. Each additional channel is opt-in, and rollback consists
of removing it from the Engine V2 channel allowlist. No database migration or
WIT change is required for the first milestone.

## Documentation Impact

After approval, the implementation plan must update
`docs/proposals/ENGINE_LLM_STREAMING.md` so its Phase 2 and Phase 3 descriptions
use channel-neutral status and response delivery. Implementation must also
check `ic/FEATURE_PARITY.md`, API documentation, and the changelog before the
feature status changes.
