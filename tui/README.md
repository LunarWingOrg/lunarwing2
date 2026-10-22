# LunarWing TUI

OpenTUI-based terminal client for a running LunarWing daemon — the modern
replacement for (and supplement to) the plain `lunarwing repl` client, with
**multimodal input** (images) and live streaming.

```
┌──────────────────────────────────────────────┐
│ LunarWing ● ready  /run/user/1000/lunarwing… │
│                                              │
│ you ▏ what is in this screenshot?            │
│      [image preview]                         │
│ lunarwing ▏ That's the ports registry…       │
│                                              │
│ ┌─ Message ────────────────────────────────┐ │
│ │ ›_                                      │ │
│ └─────────────────────────────────────────┘ │
│ Enter send · Ctrl+Enter/Ctrl+S interrupt    │
└──────────────────────────────────────────────┘
```

## Requirements

- [Bun](https://bun.sh) ≥ 1.3 (the OpenTUI Zig core is loaded through Bun's
  native interface; the packages declare `bun >=1.3.14`, 1.3.13 works in
  practice).
- A running LunarWing daemon (`lunarwing run`, systemd unit, …) with its
  Unix socket REPL (always enabled in daemon mode).
- For clipboard image paste on Linux: `wl-paste` (Wayland) or `xclip`/`xsel`
  (X11). On macOS: `pngpaste`. Optional — `/img <path>` always works.

## Run

```sh
cd tui
bun install
bun start                 # default socket resolution (see below)
bun start --socket /path/to/lunarwing.sock
bun start --image-protocol kitty   # force a graphics backend (see Images)
```

Equivalent env form: `LUNARWING_TUI_IMAGE_PROTOCOL=kitty bun start`.

Socket resolution order (mirrors `lunarwing repl`):

1. `--socket` CLI argument
2. `$LUNARWING_SOCKET`
3. `$IRONCLAW_SOCKET`
4. `$XDG_RUNTIME_DIR/lunarwing.sock`
5. `$LUNARWING_BASE_DIR/lunarwing.sock` (legacy `$IRONCLAW_BASE_DIR`)
6. `~/.ironclaw/lunarwing.sock`

## Input model

| Key | Action |
| --- | --- |
| `Enter` | Send. Mid-turn messages are **queued natively** by the daemon's scheduler and run after the current turn. |
| `Ctrl+Enter` / `Ctrl+S` | **Interrupt + send** (hard steer): sends the daemon's `/interrupt` priority control (aborts the running turn, keeps queued follow-ups), then your message. `Ctrl+Enter` requires a terminal with the kitty keyboard protocol; `Ctrl+S` works everywhere. |
| `/steer <note>` | Soft steer (typed literally): injects a mid-turn note delivered after the current tool call. |
| `Ctrl+V` | Paste from clipboard: image → pending attachment, text → inserted into the input. |
| `/img <path>` | Attach an image file from disk. |
| `Ctrl+R` / `Ctrl+X` | Remove last / clear pending attachments. |
| `Ctrl+C` | Quit (graceful disconnect). |

Pending attachments render as inline previews (Kitty graphics / Sixel /
Unicode-block fallback, negotiated by OpenTUI) and are sent as base64
attachments with the next message. Sent images stay in the transcript, so a
turn remains reviewable after it seals.

## Images (both directions)

The terminal image protocol is negotiated by OpenTUI (`protocol: "auto"`),
which resolves per terminal to **Kitty graphics**, **Sixel**, or a
unicode-block fallback — no custom OSC 5522 code lives in this app. The header
shows the resolved choice as `img:kitty` / `img:sixel` / `img:blocks` (green
when Kitty is active), so a silent fallback is visible instead of mysterious.

- **Input:** `Ctrl+V` (clipboard via `wl-paste` / `xclip` / `xsel` /
  `pngpaste`) or `/img <path>`, magic-byte MIME sniffed, no dependencies.
- **Output:** `image_generated` status frames from the daemon (the
  `image_generate` / `image_edit` tools) carry a base64 data URL that is
  decoded and rendered inline — both while the turn streams and after it
  seals into a transcript item.
- Inline images draw at up to 56 columns wide with a caption line, and count
  against the transcript's tail-view height budget.

The live→sealed handoff is pinned by `test/image-output.test.tsx`: an image
must survive the done frame, exactly like streamed text does.

### Forcing a protocol (tmux!)

OpenTUI refuses Kitty graphics whenever it detects a multiplexer
(`multiplexer === "tmux"` short-circuits to unicode blocks), on the grounds
that graphics escapes can be swallowed by the pane. That is the safe default,
but it means a modern tmux in front of a graphics-capable terminal shows
blocks even though pixels would work.

To test the real thing:

```sh
# 1. let tmux forward graphics escapes (tmux >= 3.3; 3.4+ also accepts "all")
tmux set -g allow-passthrough on
#    persist it: echo 'set -g allow-passthrough on' >> ~/.tmux.conf

# 2. force the backend past OpenTUI's multiplexer guard
bun start --image-protocol kitty
```

The header's `img:` label shows the *effective* backend, so you can see at a
glance whether the override took and whether the graphics actually survive the
multiplexer. Forcing `kitty` in a terminal that cannot render it produces
garbage rather than images — that is why `auto` stays the default.

`--image-protocol` accepts `auto`, `kitty`, `sixel`, or `blocks`. Any other
value is ignored with a warning on stderr.

## Development

```sh
bun run typecheck   # tsc --noEmit
bun test            # unit + protocol + input-routing tests
```

## Protocol

Speaks the daemon's Unix-socket REPL NDJSON protocol
(`ic/src/channels/unix_socket_repl.rs`) with two v2 extensions, both
backward-compatible:

- `Connect.features: ["status"]` — opts the client into live `Status` frames
  (stream chunks, thinking/tool progress, generated images). Legacy clients
  never receive them.
- `Message.attachments: [{ mime_type, filename, data_base64 }]` — multimodal
  input, decoded server-side into channel attachments (≤ 8 per message,
  ≤ 10 MiB each, ≤ 25 MiB total per message).

### Streaming invariant (hard-won lesson)

**The done frame closes the stream; it never wipes it.** The client
accumulates `stream_chunk` deltas; the final `Response { is_complete: true }`
adopts its payload as canonical *only when non-empty* — an empty done frame
keeps everything accumulated. Late chunks after the done frame are dropped,
and a duplicate done frame changes nothing. Pinned by regression tests in
`test/turn-buffer.test.ts`.

## Layout

```
src/
  main.tsx                    entry: renderer + socket resolution
  app.tsx                     state, key handling, submission model
  repl-client.ts              NDJSON Unix-socket client (handshake, keepalive)
  protocol.ts                 wire types mirrored from the Rust side
  turn-buffer.ts              stream accumulation state machine (invariant)
  images.ts                   clipboard paste, file attach, MIME sniffing
  util.ts                     wrap / data-url helpers
  components/
    transcript.tsx            tail-view transcript + live turn
    attachment-bar.tsx        pending attachment previews
test/
  turn-buffer.test.ts         streaming invariant regression tests
  repl-client.test.ts         protocol tests against a mock daemon
  input-keys.test.tsx         OpenTUI key-routing behavioral pins
  images.test.ts              MIME sniffing / file / data-url tests
  image-output.test.tsx       generated-image render + live→sealed handoff
```
