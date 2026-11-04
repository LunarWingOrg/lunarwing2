# Gateway Chat API (chat contract, `api_version` 1)

Normative description of the web gateway's chat surface as a machine client
uses it: Wingtip, the multimodal terminal client (`tui/`), is the reference
consumer, and the web UI uses a subset. Version 1 of the contract is milestone M2a of
the TUI design ("gateway contract in the daemon"): message correlation, turn
lifecycle events, the capabilities document, image validation, Engine V2
generated images and the `/quit` refusal. Client tokens (M2b) are not part of
this version; every authenticated request uses the master credential.

Source anchors (locate by name; line numbers drift):

| Area | Source |
| --- | --- |
| Routes and handlers | `ic/src/channels/web/server.rs`: `chat_send_handler`, `chat_capabilities_handler`, `chat_approval_handler`, `chat_auth_token_handler`, `chat_auth_cancel_handler`, `queue_engine_auth_resume`, `gateway_message_metadata`, `gateway_image_attachments`, `chat_events_handler` |
| Event stream | `ic/src/channels/web/sse.rs`: `SseManager::broadcast_correlated`, `Correlation`, `sse_payload` |
| Wire types | `ic/src/channels/web/types.rs`: `SseEvent`, `SendMessageRequest`, `ApprovalRequest`, `AuthTokenRequest`, `AuthCancelRequest`, `ChatCapabilitiesResponse` |
| Channel mapping | `ic/src/channels/web/mod.rs`: `GatewayChannel::{respond, send_status, message_lifecycle}`, `status_sse_event`, `CHAT_RATE_LIMIT`, `VOICE_RATE_LIMIT` |
| Shared vocabulary | `ic/crates/lunarwing_common/src/client_caps.rs`: `is_valid_client_id`, `TurnOutcome`, `WireLimits`, `FEATURES_CANONICAL` |
| Lifecycle hook | `ic/src/channels/lifecycle.rs`, `ChannelManager::{message_lifecycle, note_turn_outcome}`; semantics in [OUTBOUND-DELIVERY-SEMANTICS.md](../architecture/OUTBOUND-DELIVERY-SEMANTICS.md#message-lifecycle-and-turn-outcomes) |
| Image validation | `ic/src/channels/image_mime.rs`: `decode_images`, `GATEWAY_IMAGE_LIMITS` |
| Golden event file | `ic/src/channels/web/tests/fixtures/sse_current.ndjson` (see [Testing](#testing)) |
| Voice routes | [GATEWAY_VOICE_API.md](GATEWAY_VOICE_API.md) |

Key words: **must**, **must not** and **should** are requirements on clients
unless a sentence names the server.

## Detecting the contract

`GET /api/chat/capabilities` exists exactly on daemons that implement this
contract. A **404** means a daemon without it ("legacy"): no correlation
fields, no `turn_started`/`turn_finished`, invalid images silently dropped
instead of refused, no generated images on Engine V2, and `/quit` from the
gateway stops the daemon. A client must degrade (FIFO matching, settle
heuristics) and must still never send the commands of
[Inputs a client must not send](#inputs-a-client-must-not-send-as-chat).

Evolution is additive. Clients must ignore unknown event types and unknown
fields, must treat an unknown `outcome` as "unknown", and must accept
`features` entries they do not know.

## Authentication and error bodies

- Every route below is protected: `Authorization: Bearer <token>`, scheme
  case-insensitive, compared as a SHA-256 digest in constant time
  (`MultiAuthState::authenticate`).
- `?token=` is accepted only on `GET /api/chat/events`, `/api/logs/events`
  and `/api/chat/ws` (for browsers' `EventSource`). Machine clients must not
  use query tokens: URLs end up in logs and process listings.
- Error bodies:
  - auth middleware: plain-text **401** `Invalid or missing auth token`;
  - chat handlers: plain text;
  - axum extractor rejections: plain text (**400** malformed JSON, **413**
    body over the limit, **415** missing `Content-Type: application/json`,
    **422** a field of the wrong shape, for example a non-string
    `client_turn_id`);
  - voice handlers: JSON `{"error","message"}` (see the voice guide).

  Clients must branch on `Content-Type` before parsing an error body.

## Shared vocabulary

### Client turn ids

The client generates one id per user turn and per control (`/interrupt`,
`/steer`), normally `crypto.randomUUID()`, and sends it as
`client_turn_id`.

- Syntax: `^[A-Za-z0-9_-]{1,64}$` (`is_valid_client_id`). An invalid id is
  rejected with **400** `invalid client_turn_id`, never rewritten.
- Ids are opaque, never parsed, and not secrets. `message_id` is a server
  UUID (`IncomingMessage::id`), also returned by the 202 of `send` and
  `approval`.

### Correlation rules

1. Every SSE event caused by a message the chat handlers stamped carries
   top-level `message_id` and, when the client sent one, `client_turn_id`,
   after the event's own fields (`type` stays first).
2. The ids are echoed on progress, approval prompts, image events, the
   reply, lifecycle events and errors of that message.
3. An approval decision and a credential resume the **original** turn: the
   client sends the owning turn's `client_turn_id` with the approval or the
   credential, and the continuation's events carry it (with the
   continuation's own `message_id`).
4. An event without ids is out of band: proactive `broadcast` replies,
   `message`-tool sends, routine and job notices, `gate_resolved`, extension
   status. Clients show it as a background item and must not FIFO-match it to
   a pending turn.
5. Events are scoped to the authenticated user (plus global events such as
   `heartbeat`); the client filters by `thread_id`.

### Turn outcomes

`turn_finished.outcome` is `lunarwing_common::TurnOutcome` in snake case:

| `outcome` | Meaning |
| --- | --- |
| `replied` | A non-empty reply was delivered before this event. |
| `no_reply` | Handling ended without a reply: suppressed or silent reply (`NO_REPLY`), a consumed control, a stale or bare approval keyword, a hook rejection (an inbound hook, or an outbound hook that rejected the reply, failed or emptied it). |
| `paused` | The turn stopped at a gate (approval, credential, external callback). A continuation follows the decision with its own `turn_started` and finish. |
| `interrupted` | A running turn was stopped by `/interrupt` (or `/stop`); on Engine V2 also an approval continuation whose approved action was still running. |
| `detached` | The turn outlived its wait budget and continues in the background. Not terminal: exactly one more `turn_finished` follows when it ends. |
| `failed` | The handler returned an error or an `Error: ...` reply or panicked, the engine reported failure (including max iterations), or the input was refused (see [Limits](#limits-wirelimits)). |
| `timed_out` | The agent loop's soft timeout fired; the timeout notice was sent. Terminal: an Engine V2 turn that keeps running may still deliver a late `response`, but no further `turn_finished`. |
| `cancelled` | The message did not complete: removed from the queue by an interrupt or refused by a full queue (it never ran), or cancelled by daemon shutdown while it ran. |

Readers map any other string to "unknown" (`#[serde(other)] Unknown`;
producers never emit it).

### Limits (`WireLimits`)

| Field | Gateway value |
| --- | --- |
| `max_content_bytes` | 100000 (the safety validator's default maximum) |
| `max_attachments` / `max_attachment_bytes` / `max_total_attachment_bytes` | 5 / 5242880 / 7340032 (decoded bytes) |
| `attachment_mime_types` | `image/png`, `image/jpeg`, `image/gif`, `image/webp` |
| `max_transcribe_b64_bytes` | 25165824 when voice is configured, else 0 |
| `transcribe_formats` | `wav ogg opus mp3 mp4 webm flac m4a aac` when voice is configured, else empty |
| `messages_per_minute` / `transcribes_per_minute` | 30 / 10, per user, shared with the web UI |
| `max_request_bytes` | 10485760 (the global request body limit) |
| `max_frame_bytes`, `max_in_flight`, `idle_timeout_secs` | absent (socket-only fields) |

Content over `max_content_bytes` is not refused by the HTTP layer (only by
the 10 MiB body limit): the agent's input validation rejects it and the turn
finishes `failed`. Engine V1 replies `Error: Input rejected by safety
validation: ...`; Engine V2 replies the same text without the `Error: `
prefix. Input that a blocking safety policy rule or the secret scanner refuses
ends the same way (V1 `Error: <reason>`, V2 the bare reason). On Engine V2 the
validated text includes the `<attachments>` block that describes attached
images, so content just under the limit plus images can still be refused.
Clients enforce the limit before sending.

## `GET /api/chat/capabilities`

Protected, not rate-limited.

```json
{
  "api_version": 1,
  "server_version": "2.1.8",
  "engine": "v2",
  "features": ["status", "attachments", "transcribe", "stream"],
  "limits": {
    "max_content_bytes": 100000,
    "max_attachments": 5,
    "max_attachment_bytes": 5242880,
    "max_total_attachment_bytes": 7340032,
    "attachment_mime_types": ["image/png", "image/jpeg", "image/gif", "image/webp"],
    "max_transcribe_b64_bytes": 25165824,
    "transcribe_formats": ["wav", "ogg", "opus", "mp3", "mp4", "webm", "flac", "m4a", "aac"],
    "messages_per_minute": 30,
    "transcribes_per_minute": 10,
    "max_request_bytes": 10485760
  },
  "voice": {"provider": "lunarvoice", "stt": true, "tts": true},
  "credential": "master"
}
```

- `server_version`: the daemon's package version.
- `engine`: `"v2"` when Engine V2 routing is enabled (`ENGINE_V2`), else
  `"v1"`. Gateway turns route through V2 only when the engine is enabled.
- `features`, always in the canonical order `status, attachments, transcribe,
  stream, client_tokens`: `status` and `attachments` always; `transcribe` when
  the voice API is configured (`TRANSCRIPTION_PROVIDER=lunarvoice` plus
  `VOICE_AUTH_TOKEN`); `stream` when `engine` is `"v2"` (`stream_chunk`
  events are produced only by Engine V2); `client_tokens` only when the daemon
  loads `state/gateway-client-tokens.json` and authenticates the client tokens
  registered there (M2b; never in this version). `mt-admin tui-access` reads
  this document with the master token before minting a client token and
  refuses, writing nothing, when the feature is absent or the route answers
  404.
- `voice.provider`: `"lunarvoice"` when the voice API exists, else `null`, and
  then `stt` and `tts` are `false`.
- `credential`: always `"master"` in this version (M2b adds `"client"`).
- The document's existence implies everything else in this guide.

## Threads

- Create: `POST /api/chat/thread/new` (no body) answers
  `{"id","state","turn_count","created_at","updated_at","thread_type","channel"}`.
- List: `GET /api/chat/threads` answers `{"assistant_thread", "threads",
  "active_thread"}`. `active_thread` is a legacy V1 pointer; clients ignore it.
- There is no server-side "select thread". Clients must use UUID thread ids
  from these endpoints: Engine V2 adopts the UUID as its engine thread id, so
  SSE `thread_id` equals the client's thread id. A non-UUID id makes V2 mint
  its own id, and history answers 400.

## `POST /api/chat/send`

```json
{
  "content": "what is in this screenshot?",
  "thread_id": "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10",
  "timezone": "Europe/Berlin",
  "images": [{"media_type": "image/png", "data": "iVBORw0KGgoAAAANSUhEUgAA..."}],
  "client_turn_id": "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e"
}
```

- `content` is required (it may be `""` when images are attached).
- `thread_id` is required: **400** `thread_id is required for /api/chat/send`.
- `timezone` is optional; the `X-Timezone` header is the fallback.
- `client_turn_id` is optional; invalid: **400** `invalid client_turn_id`.
- `images` (optional): see [Images](#images).
- Checks run in this order: `thread_id`, `client_turn_id`, rate limit, images.
  A request refused before the rate limit does not consume a slot; a request
  with an invalid image does.
- Response: **202** `{"message_id":"<uuid>","status":"accepted"}`.
- Errors: **400** as above; **429** `Rate limit exceeded. Try again shortly.`
  (30 per 60 s per user, shared with the web UI; controls count too);
  **413**/**422** for images; **503** `Channel not started`; **500**
  `Channel closed`; extractor rejections (400, 413, 415, 422).

The daemon queues `IncomingMessage::new("gateway", user_id, content)` on the
thread with this metadata (`gateway_message_metadata`):

```json
{"user_id": "derpy", "thread_id": "5f0e4a52-…", "message_id": "0b8a6c1e-…", "client_turn_id": "0193f2a1-…"}
```

Scheduling: Engine V2 user input from the gateway runs in parallel lanes
keyed by `(channel, user, thread)`, at most 4 at a time, and messages of one
thread run in order. Everything else (Engine V1, approvals, commands) runs one
message at a time in arrival order, except the priority controls
`/interrupt`, `/stop` and `/steer`, which are handled at once.

### Images

`images[]` carries base64 without a `data:` prefix. The server validates the
whole list with `decode_images(GATEWAY_IMAGE_LIMITS, "web-image")` before the
message is queued, in this order:

1. at most 5 images;
2. the decoded total that the base64 lengths imply (exact for valid,
   padded base64) is at most 7 MiB;
3. per image: the base64 length fits 5 MiB decoded; the declared
   `media_type` starts with `image/` (case-insensitive); the data decodes; it
   is not empty; it is at most 5 MiB; the running total is at most 7 MiB;
4. per image: the type is sniffed from the magic bytes and must be PNG,
   JPEG, GIF (87a/89a) or WebP. SVG, BMP, TIFF, HEIC, AVIF and ICO are
   refused.

The sniffed type replaces the declared one. Attachment ids are
`web-image-<uuid v4>`; file names are `image-<n>.<ext>` (1-based). Any
rejection fails the whole request with a plain-text body naming the image
(1-based):

| Status | Body |
| --- | --- |
| 422 | `image 6 rejected: too many images (6, at most 5)` (names the first image over the limit) |
| 413 | `image 2 rejected: larger than 5242880 bytes` |
| 413 | `images rejected: the images exceed 7340032 bytes in total` |
| 422 | `image 1 rejected: invalid base64` |
| 422 | `image 1 rejected: unsupported image type (allowed: png, jpeg, gif, webp)` |
| 422 | `image 1 rejected: no image data` |

Lengths are checked before decoding, so an oversize payload answers 413 even
when its base64 is also invalid. Clients pre-validate type and sizes with
the advertised limits and should display the body as is. Every image sent is
stored in the tenant workspace (the TUI discloses this); there is no expiry.

## `GET /api/chat/events` (SSE)

- `text/event-stream`. Each frame is `event: <type>` plus one
  `data: <json>` line whose object carries the same `type`. An empty comment
  every 15 s keeps idle streams open; clients should reconnect after about
  45 s without bytes.
- Scope: events for the authenticated user plus global events. One stream
  per client is enough; filter by `thread_id`.
- Cap: 100 connections per gateway, SSE streams and WebSockets together. A
  new SSE stream beyond it answers **503** `Too many connections`; a
  WebSocket beyond it is upgraded (101) and then closed at once.
- Lag: the broadcast buffer holds 256 events. A subscriber that falls behind
  receives `stream_gap {"missed_events": n}` and continues. There is no `id:`
  field and no `Last-Event-ID`; recover by resynchronizing (history and
  pending gates).
- Size: `image_generated` carries a whole data URL (up to 16 MiB). Parsers
  must accept data lines of at least 24 MiB and decode UTF-8 across chunk
  boundaries.
- Open the stream before the first `send`, so that no event of the turn is
  missed.

### Event catalog

"Corr" means the event carries `message_id` and `client_turn_id` when a
stamped message caused it.

| `type` | Fields | Producer | Corr |
| --- | --- | --- | --- |
| `turn_started` | `thread_id` | lifecycle `Started` | yes |
| `turn_finished` | `thread_id`, `outcome` | lifecycle `Finished`, after the reply | yes |
| `response` | `content`, `thread_id`, `emotes?` | the reply (`respond`); proactive `broadcast`, mission results | yes for replies, no for proactive |
| `thinking` | `message`, `thread_id?` | V1 and V2 status text | yes |
| `stream_chunk` | `content`, `thread_id?` | V2 only; the final `response.content` is canonical | yes |
| `tool_started` / `tool_result` / `tool_completed` | `name` / `name, preview` / `name, success, error?, parameters?` | V1 and V2 (V2 previews at most 1000 bytes; on V2 `parameters` is the duration, for example `"8123ms"`) | yes |
| `image_generated` | `data_url`, `path?`, `thread_id?` | V1 and V2 image tools; replaces the tool's `tool_result` | yes |
| `reasoning_update` | `narrative`, `decisions[{tool_name, rationale}]`, `thread_id?` | V1, V2 | yes |
| `status` | `message`, `thread_id?` | progress and misc (skills, external waits); an empty `message` must be ignored | yes |
| `approval_needed` | `request_id`, `tool_name`, `description`, `parameters` (redacted, pretty JSON), `thread_id?`, `allow_always` | V1, V2 | yes |
| `gate_required` | `request_id`, `gate_name`, `tool_name`, `description`, `parameters`, `resume_kind`, `thread_id?` | V2; deduplicate with `approval_needed` by `request_id` | yes |
| `gate_resolved` | `request_id`, `gate_name`, `tool_name`, `resolution`, `message`, `thread_id?` | V2 | no |
| `auth_required` / `auth_completed` | `extension_name`, `instructions?`, `auth_url?`, `setup_url?` / `extension_name`, `success`, `message` | agent status: correlated; the `auth-token` handler: not correlated | partly |
| `turn_cost`, `suggestions` | as named | V1 only | yes |
| `thread_state_changed`, `turn_recovery`, `child_thread_spawned` | as named | V2 | yes |
| `job_started` | `job_id`, `title`, `browse_url` | a job created during a turn | yes |
| `job_message`, `job_tool_use`, `job_tool_result`, `job_status`, `job_result` | as named | the job event bus | no |
| `extension_status`, `skill_activated`, `error`, `heartbeat`, `stream_gap` | as named | extensions, unused, unused, global, lag | no |

Typical Engine V2 turn with one image tool call:

```text
event: turn_started          data: {"type":"turn_started","thread_id":"T","message_id":"M","client_turn_id":"C"}
event: thinking              data: {"type":"thinking","message":"Processing...","thread_id":"T","message_id":"M","client_turn_id":"C"}
event: thread_state_changed  data: {"type":"thread_state_changed","thread_id":"T","from_state":"Created","to_state":"Running",…corr}
event: stream_chunk          data: {"type":"stream_chunk","content":"Let me draw","thread_id":"T",…corr}
event: tool_started          data: {"type":"tool_started","name":"image_generate","thread_id":"T",…corr}
event: image_generated       data: {"type":"image_generated","data_url":"data:image/png;base64,iVBOR…","thread_id":"T",…corr}
event: tool_completed        data: {"type":"tool_completed","name":"image_generate","success":true,"parameters":"8123ms","thread_id":"T",…corr}
event: response              data: {"type":"response","content":"Here is the image.","thread_id":"T",…corr}
event: turn_finished         data: {"type":"turn_finished","thread_id":"T","outcome":"replied","message_id":"M","client_turn_id":"C"}
```

Generated images: the data URL starts with
`data:image/{png|jpeg|gif|webp};base64,` (the prefix is lowercased by the
server), has a non-empty body and is at most 16 MiB in total. The server does
not sniff generated bytes; clients must decode and sniff them before
rendering, and must never load `path` (a caption, not a source). Both engines
parse the image tools' output with one parser (`parse_image_sentinel`).
Before this contract, Engine V1 never recognised the real tool output (it
arrives JSON-encoded) and showed it as a `tool_result` preview, and Engine V2
had no `image_generated` at all; now both send `image_generated` and no
preview. Exception: actions run through the engine's parallel batch path
(CodeAct `__execute_actions_parallel__`) still show the preview.

### Turn lifecycle

`turn_started` and `turn_finished` are emitted only for messages that carry
a `message_id` from the chat handlers (`send`, `approval`, the V2 credential
resume) and a thread. Internal messages (job monitor, routines) and
WebSocket sends emit neither. The rules (the daemon-side contract is in
[OUTBOUND-DELIVERY-SEMANTICS.md](../architecture/OUTBOUND-DELIVERY-SEMANTICS.md#message-lifecycle-and-turn-outcomes)):

1. `turn_started` when the agent dequeues the message (immediately for
   priority controls), then progress, then the `response` if any, then
   exactly one `turn_finished`. The reply is always broadcast before the
   finish of its message.
2. `detached` is followed by exactly one more `turn_finished` (the late
   result); a client keeps the turn open in the background until then.
   `timed_out` is never followed by another finish: when an Engine V2 turn
   outlives the soft timeout, its late reply arrives as a correlated
   `response` only.
3. `paused` is followed, after the decision, by the continuation's
   `turn_started` (new `message_id`, the original `client_turn_id`) and its
   own finish. On Engine V2 the uncorrelated `gate_resolved` of the decision
   comes after that `turn_started`.
4. Some messages finish without starting: queued prompts removed by an
   interrupt (`cancelled`), and a message refused because the deferred queue
   (256 messages) is full (`response` "Agent is busy and its deferred message
   queue is full. Try again after the active turn finishes." then
   `cancelled`).
5. Messages still queued when the daemon exits get no finish; a client
   resynchronizes on reconnect.
6. Clients should still tolerate a finish before the reply and a duplicate
   finish for a sealed turn (legacy or racing producers).

## Approvals and gates

- Prompts: `approval_needed` (always) and `gate_required` (Engine V2).
  After a reconnect, `GET /api/chat/pending-gates` answers
  `{"gates":[{"request_id","thread_id","gate_name","tool_name","description","parameters","resume_kind"}]}`,
  and history carries `pending_approval`.
- Decide with `POST /api/chat/approval`:

```json
{"request_id": "6c0f2a8e-1f9b-4d3e-a1c2-7b5e9d0f4a21", "action": "approve",
 "thread_id": "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10", "client_turn_id": "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e"}
```

  - `action`: `approve`, `always` or `deny`. `client_turn_id` is the
    **owning turn's** id. Answer: **202** `{"message_id","status":"accepted"}`.
  - The approval is queued as a message with the same metadata as a send, so
    the continuation's events are user-scoped and correlated to the owning
    turn, with their own lifecycle events.
  - Errors (plain text): **400** `Unknown action: <action>`,
    `Invalid request_id (expected UUID)`, `invalid client_turn_id`,
    `thread_id must not be blank when supplied`,
    `thread_id does not own this approval request`; **404**
    `No pending approval matches this request_id`; **503**
    `Failed to inspect pending approvals`. Not rate-limited.
- Approvals resolve against the authenticated owner's gates only. A client
  holds the same approval authority as the web UI.

## Credentials (`auth-token`, `auth-cancel`)

- `POST /api/chat/auth-token`
  `{"extension_name","token","request_id?","thread_id?","client_turn_id?"}`
  and `POST /api/chat/auth-cancel`
  `{"extension_name","request_id?","thread_id?","client_turn_id?"}`.
  `request_id` and `thread_id` are UUIDs and go together (**400**
  `request_id and thread_id must be supplied together`); a named gate that is
  no longer pending answers **409**
  `Authentication request is stale or no longer available`.
- `client_turn_id` is the owning turn's id, validated like `send`. When an
  Engine V2 authentication gate for the extension is pending, a successful
  `auth-token` queues a resume message carrying the gateway metadata (its own
  `message_id`, the sent `client_turn_id`), so the resumed turn's events and
  its `turn_started`/`turn_finished` belong to the owning turn.
- Otherwise (Engine V1, or no V2 gate), and for `auth-cancel` on both
  engines, the request is handled outside the agent loop: no lifecycle
  events. The handler announces the result with an uncorrelated
  `auth_completed` (or `auth_required` to retry); on V2 a cancelled gate is
  announced by `gate_resolved{"resolution":"cancelled"}`. A client seals the
  owning turn when the POST returns 2xx.
- The credential travels only in this request body; it is never echoed in an
  event. Answers are `{"success","message",...}` with 200, also when
  `success` is false.

## Interrupt and steer

There is no cancel endpoint. Interrupt with `POST /api/chat/send
{"content":"/interrupt","thread_id":"T","client_turn_id":"<new id>"}`
(`/stop` is equivalent). It is a priority control handled at once; on Engine
V2 it stops every running thread of the conversation. The control gets
`response` "Interrupted." or "Nothing to interrupt." and
`turn_finished{replied}` under its own id; the interrupted turn finishes
`interrupted` (on V1 it also gets a `response` "Interrupted."); queued prompts
of the lane removed by the interrupt finish `cancelled`. On Engine V2 an
approval continuation whose approved action is still running is interrupted
too: the action's effects stand, the thread is not resumed, and the
continuation finishes `interrupted`. Steer with
`/steer <note>`; its acknowledgement is a `response` under the steer's id.
Both count against the chat rate limit.

## History

`GET /api/chat/history?thread_id=<uuid>&limit=50&before=<rfc3339>` answers
`{"thread_id","turns":[{"turn_number","user_input","response","state","started_at","completed_at","tool_calls","reasoning"}],"has_more","oldest_timestamp","pending_approval?"}`.
Durable V2 history shows an in-flight turn as `Failed`; `user_input` is the
effective content (with an `<attachment>` block for images); history carries
no `message_id` or `client_turn_id`.

## Inputs a client must not send as chat

- `/quit`, `/exit`, `/shutdown`: refused by the daemon for the `gateway` (and
  `unix_socket_repl`) channel. The turn gets `response` "Error: /quit, /exit
  and /shutdown cannot stop the LunarWing daemon from this channel. Close the
  client instead; operators use the service manager
  (lunarwing-mt-admin.sh stop-tenant)." and `turn_finished{failed}`; the
  daemon keeps running. Legacy daemons stop on these commands, so clients
  intercept them locally on every daemon.
- A silent reply (a V1 reply that `is_silent_reply` reads as silent:
  `NO_REPLY`, optionally followed by up to four whitespace or ASCII
  punctuation characters, for example `NO_REPLY.`; or Engine V2
  `Completed{response: None}`) finishes `no_reply` on every channel (WI#550).
  Both producers use the empty-response suppression sentinel, so the daemon
  keeps running and no assistant history row is written. The DEC-1 guard on
  the two client channels remains as a backstop for any other `Ok(None)`.
- `/restart`: an operator action (`lunarwing-mt-admin.sh restart-tenant`); the
  TUI refuses it locally.
- Bare approval keywords (for example `yes`, `ok`, `no`, `deny`, `always`;
  the full list is in `SubmissionParser::parse`) with no prompt pending: the
  daemon consumes them as a control and answers `turn_finished{no_reply}`.
  Clients confirm before sending them.

## WebSocket (`/api/chat/ws`)

The WebSocket path serves browser-origin clients; the bundled web UI does not
use it (it uses SSE and `fetch`), and machine clients use SSE plus
`POST /api/chat/send`. The upgrade must carry an `Origin` header whose host
is `localhost`, `127.0.0.1` or `[::1]`: without one the gateway answers
**403** `WebSocket Origin header required`, with another host **403**
`WebSocket origin not allowed`. Over the connection cap the upgrade still
succeeds (101) and the socket is closed at once; there is no 503. It has no
rate limit, its messages carry no correlation metadata (so no lifecycle
events), and its events never carry `message_id`/`client_turn_id`. Images
sent over it are validated like `send`; a rejection answers a WebSocket
`error` frame with the same text and the message is not forwarded.

## Testing

- Golden event file: `ic/src/channels/web/tests/fixtures/sse_current.ndjson`,
  one `{"event": <name>, "data": <payload>}` line per frame, with the payload
  bytes exactly as sent on the `data:` line. It is produced by the real
  channel methods and read back from a real SSE response body, covers every
  `SseEvent` variant, and fails `sse_current_fixture_matches_server_output`
  when the server output changes. Regenerate and review the diff with
  `LW_REGEN_FIXTURES=1 taskset -c 0-11 cargo test -j12 --lib -- channels::web::tests::correlation`.
  Clients check their decoder against it; for M2a every line was fed through
  `tui/src/transport/gateway-wire.ts` (`parseGatewayEvent` and
  `mapGatewayEvent`, as `gateway.ts` calls them) without a rejected or
  misread event. A `bun test` that replays the file in `tui/` is a follow-up.
- Server tests: `cargo test --lib -- channels::web` (correlation, lifecycle,
  images, capabilities, voice), `--test e2e_turn_lifecycle` and
  `--test e2e_quit_refusal` (`--features libsql`).
- Client tests: `cd tui && bun test` runs the mock gateway in `legacy` and
  `current` modes.
