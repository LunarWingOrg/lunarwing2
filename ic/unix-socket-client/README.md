# LunarWing Unix Socket Client

Standalone REPL client for a running LunarWing daemon, speaking protocol 1,
the newline-delimited JSON protocol of the daemon's Unix socket REPL. The
REPLv3 vocabulary and golden compatibility fixtures live in
`ic/crates/lunarwing_common/src/repl_wire.rs` and
`ic/crates/lunarwing_common/tests/fixtures/replv3/`.

Protocol-1 approval prompts and no-reply notices arrive as ordinary `Response`
frames so this lockstep client cannot wait forever for a reply. Attachments
require REPLv3 and are refused with a legacy error response. This standalone
client does not inspect socket ownership before connecting; use the daemon's
private runtime directory and the main `lunarwing repl` client when that check
is required.

Lives in-tree at `ic/unix-socket-client/`. Not a workspace member; build it
directly from this directory.

Default socket resolution mirrors the daemon: `$LUNARWING_SOCKET` (legacy
`$IRONCLAW_SOCKET`), then `$XDG_RUNTIME_DIR/lunarwing.sock`, then
`<base_dir>/lunarwing.sock`. Pass an explicit socket path as the first
argument to override.
