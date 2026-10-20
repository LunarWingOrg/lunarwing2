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
```

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
attachments with the next message.

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
```
