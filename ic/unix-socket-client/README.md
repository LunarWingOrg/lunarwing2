# LunarWing Unix Socket Client

Standalone REPL client for a running LunarWing daemon, speaking the
newline-delimited JSON protocol of the daemon's Unix socket REPL.

Lives in-tree at `ic/unix-socket-client/`. Not a workspace member; build it
directly from this directory.

Default socket resolution mirrors the daemon: `$LUNARWING_SOCKET` (legacy
`$IRONCLAW_SOCKET`), then `$XDG_RUNTIME_DIR/lunarwing.sock`, then
`<base_dir>/lunarwing.sock`. Pass an explicit socket path as the first
argument to override.

## Protocol note

This is the plain-text legacy client. The protocol has grown two
backward-compatible extensions (see `ic/src/channels/unix_socket_repl.rs`
for the authoritative definition):

- `Connect { features: ["status"] }` — opts a client into live `Status`
  frames (stream chunks, tool progress). This client does not opt in and so
  never receives them.
- `Message { attachments: [...] }` — base64-encoded multimodal input. This
  client sends text only (empty attachment list).

For the multimodal OpenTUI client with image input, previews, and streaming,
use `tui/` at the repository root (`bun start`).

