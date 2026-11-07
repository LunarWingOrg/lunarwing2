# Wingtip: setup and first session

Wingtip (`tui/`) is LunarWing's terminal client. It sends text, images and push-to-talk dictation to a
tenant's web gateway and shows replies, tool activity, approvals and generated images. This guide goes
from a running tenant to a working session in four steps and then covers the jobs that come up later.

The full reference is [`tui/README.md`](../../tui/README.md): every flag, key, command, config file,
the voice and image rules, and a longer troubleshooting table. The design is
[`docs/proposals/WINGTIP_DESIGN.md`](../proposals/WINGTIP_DESIGN.md).

**Who runs what.** Wingtip runs as your desktop user, the account that owns the microphone, the
clipboard and the terminal. That account cannot read tenant files or tokens, so an operator with sudo
grants it access to one tenant at a time (step 2). The sudo commands below are the operator's; every
other command runs as the desktop user.

## Before you start

- **The tenant runs on this host.** `tui-access` writes a `127.0.0.1` gateway URL, and Wingtip checks
  that the tenant's own uid holds that port before it sends the token.
- **An mt-admin checkout from master** that includes MR !2173 (merged 2026-11-03), which added
  `tui-access`, `rotate-gateway-token` and the Wingtip client. Run every mt-admin command below from it.
- **Bun 1.3.14 or newer.** Node is not supported.
- **A terminal.** kitty works best: it reports key releases, so you can hold a key to talk, and it can
  show images. tmux works, with the differences listed in the README under
  [push-to-talk environments](../../tui/README.md#push-to-talk-environments).
- **Optional desktop tools**, each needed only by one feature: `wl-paste`/`wl-copy` (clipboard),
  `grim` and `slurp` (`/shot`), `notify-send` (approval notifications), `xdg-open` (`/image open`),
  `parec` (recording fallback), Hex (local transcription). A missing tool disables only its feature,
  with a message that says so.

## 1. Put the tenant on a current build (operator)

Wingtip works with any tenant daemon, but a daemon built before MR !2173 runs it in **legacy mode**:
replies are matched to your messages in order instead of by id, and generated images are not
delivered. A daemon built from master after the merge runs it in **exact mode**. Wingtip's header
shows which one you got (`exact` or `legacy daemon`).

To upgrade a tenant (the build is long, so run it in tmux):

```bash
cd <mt-admin checkout on master>
git pull
sudo ic/scripts/lunarwing-mt-admin.sh upgrade-tenant <tenant> --target master
```

`upgrade-tenant` backs up the tenant's Postgres first and puts the old build back if a step fails.
`sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>` shows the result.

## 2. Grant access (operator)

```bash
sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --for-user <user> --use-master-token
```

For example, `tui-access ruffles --for-user sun --use-master-token`. The command prints a warning,
then a report like this (it never prints a token):

```text
tui-access: granted tenant 'ruffles' to user 'sun'
  url:     http://127.0.0.1:10000
  config:  /home/sun/.config/wingtip/tenants/ruffles.json (0600, owner sun)
  token:   the tenant master token (GATEWAY_AUTH_TOKEN, token_kind master); nothing registered tenant-side
  expires: never
  owner:   port 10000 is held by uid 1010 (ruffles); recorded as tenant_uid
  verify:  ok (GET /api/gateway/status -> 200)
```

If the gateway is down, for example because the tenant is still starting after step 1, the `verify`
line says so and the file is kept; the token works once the daemon serves. Exit status: 0 done,
1 refused, 2 file written but the running daemon rejected the token.

**Why `--use-master-token`.** Today the only credential a tenant gateway accepts is its
`GATEWAY_AUTH_TOKEN`, so this mode copies that token into the desktop user's file. It has full gateway
API access (settings, extensions, logs) and does not expire. The default mode, without the flag, would
give the user a dedicated, revocable token that only reaches the chat, voice and status routes, but no
daemon accepts such tokens yet: they arrive with milestone M2b. Until then a grant without the flag is
refused and writes nothing ("the running daemon of tenant '...' does not authenticate client tokens").

Keep `~/.config/wingtip/tenants/` out of dotfile sync and backups: the file holds the token.

## 3. Install and launch (desktop user)

Install the dependencies once per checkout. The lockfile pins them exactly.

```bash
cd tui
bun install --frozen-lockfile
```

Put the `wingtip` command on your PATH. The launcher resolves its own location, so a symlink works:

```bash
ln -s "$PWD/bin/wingtip" ~/.local/bin/wingtip
```

Then start it:

```bash
wingtip --tenant <tenant>
```

Without the symlink, run `tui/bin/wingtip --tenant <tenant>` from the repository root. With exactly
one tenant file, a plain `wingtip` uses it. With several, it shows a picker.

You don't have to wait for a restarting tenant. If the gateway is not answering yet, the header shows
`reconnecting in N s` and Wingtip keeps retrying (the delay grows to at most 15 s).
`/transport reconnect` retries at once.

## 4. Your first session

The header names the tenant, the connection state (`ready` once connected), the engine, and `exact` or
`legacy daemon`. With a copied master token it also carries the warning "this TUI uses the tenant
master token". F1 opens the help overlay, and the footer always lists the keys that work right now.

| To | Do |
|---|---|
| send a message | type, then Enter; a message typed while a turn runs is queued |
| start a new line | Ctrl+J or Alt+Enter |
| stop the running turn | Ctrl+S (with text in the composer it then sends it) or `/interrupt` |
| attach an image | `/img <path>`, Ctrl+V with an image on the clipboard, or `/shot` to capture a screen region; it goes with your next Enter |
| dictate | F9 or Ctrl+Space: tap to start and stop, or hold while you talk. The text lands in the composer for you to edit; nothing is sent by voice alone |
| answer an approval | Alt+Y approve, Alt+A approve and always allow the tool, Alt+N deny; Alt+V shows the full parameters first |
| copy a reply | drag over it with the mouse, or `/copy` |
| see threads | `/thread list`; `/thread new` starts one |
| send a slash command to the daemon | `//<command>`; a single `/` is Wingtip's own command |
| quit | `/quit`, or Ctrl+C twice |

Things worth knowing on day one:

- **Approvals need a moment.** An approve key works only after the prompt has been on screen for 1 s,
  and only after Alt+V when the parameters don't fit the panel. Deny always works.
- **Images you send are stored.** The daemon keeps every image in the tenant's workspace with no
  expiry, readable from every channel. Wingtip says so the first time. It accepts PNG, JPEG, GIF and
  WebP only.
- **Voice.** `/voice` lists the transcription sources and why each one is or isn't available. The
  default order is the tenant's LunarVoice sidecar, then Hex on your machine. Hex is optional; see
  the [README](../../tui/README.md#hex-local-voice-fallback-optional).
- **`/quit` never reaches the daemon.** `/quit`, `/exit` and `/shutdown` are handled by Wingtip, or
  refused, because they would stop the tenant daemon.

## Later: rotate, revoke, upgrade

**Re-grant.** Running the same `tui-access` command again rewrites the file. When the gateway rejects
the token a running Wingtip holds, Wingtip re-reads its file once and carries on with the new token.

**Revoke one user.** This deletes the user's file:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --for-user <user> --revoke
```

With a copied master token, deleting the file does not make the token invalid. To do that, rotate it:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh rotate-gateway-token <tenant>
```

`rotate-gateway-token` writes a new `GATEWAY_AUTH_TOKEN` and restarts the tenant if it is running.
Every copy of the old token stops working: the web login (`tokens <tenant>` shows the new value), every
master-token Wingtip file, and the other uses the command lists. Re-run step 2 for every user who keeps
access.

**Tenant upgrades.** Wingtip reconnects by itself. A turn the daemon was handling when it restarted
reads "lost (daemon restarted)" and is not re-sent; ask again.

**Grants from before the rename.** Wingtip was called `lunarwing-tui` until WI#554. Files that the old
`tui-access` wrote to `~/.config/lunarwing-tui/tenants/` are no longer read, and `--revoke` no longer
looks there. Re-run step 2 for the user, then delete the old directory as that user:

```bash
rm -r ~/.config/lunarwing-tui
```

The old file held a copy of `GATEWAY_AUTH_TOKEN`. If a copy of it may have left the machine (a backup,
dotfile sync), rotate the token as above.

**When client tokens arrive (M2b).** Grant without `--use-master-token`, optionally with
`--expires-in-days 90`. `tui-access <tenant> --list` then shows the registered tokens and
`--revoke` cuts one user off within about 2 s, without a restart. After moving every user to a client
token, run `rotate-gateway-token` once so that no master-token copy stays valid.

## Protocol-1 Unix socket

The M3 hardening applies to the existing `lunarwing repl` socket; Wingtip's own socket transport
arrives with M4. The socket is mode 0600 and both server and built-in client require the peer to
have the daemon user's UID. Its parent directory must be owned by that UID and must not be
group- or other-writable. If startup reports `insecure REPL socket parent`, run
`chmod go-w <parent>` after checking the directory's ownership and contents, or configure a
dedicated owner-only directory. A missing parent is created with permissions no broader than 0700;
existing parents are never chmodded.

Keep a custom `LUNARWING_SOCKET` in a dedicated directory rather than directly under `$HOME` or
`/tmp`. Sandbox protection covers the socket's entire parent directory: putting it under `$HOME`
also prevents workspaces below `$HOME` from being sandbox mount roots. If the socket disappears or
is replaced, the daemon logs the degraded listener without marking the whole daemon unhealthy;
this deliberately avoids the restart cycle tracked by WI#385. Investigate that log before
restarting through the normal single-node or mt-admin service path.

## Troubleshooting

| You see | What to do |
|---|---|
| Wingtip exits with "no tenant access is configured for `<user>`" | Step 2 has not been run for this user, or only for the old `lunarwing-tui` directory. Files it cannot use are listed with the reason |
| `tui-access` says the daemon "does not authenticate client tokens" | Add `--use-master-token` (step 2) |
| `OFFLINE` with "the gateway rejected the token (HTTP 401 ...)" after a token rotation | Re-run step 2 for the user, then `/transport reconnect` in Wingtip |
| `OFFLINE` with "port N is held by uid U, expected X" | Another account holds the tenant's gateway port. Check the tenant with `status <tenant>` before anything else; Wingtip sent no token |
| the header says `legacy daemon` | The tenant runs a build from before MR !2173. Upgrade it (step 1) |
| "HTTP_PROXY is set: gateway requests are never sent through a proxy" | Unset the proxy variables in the shell that starts Wingtip, or set `NO_PROXY='*'` |
| `(no mic)` in the composer | No transcription source is available. `/voice` shows the reason for each |
| a reply seems to be missing | `/transport` shows the connection, the capabilities and any warnings |

More cases are in the README's [troubleshooting table](../../tui/README.md#troubleshooting).
`wingtip --debug-log` writes a metadata-only log (event types, sizes, timings, status codes; never
text, audio, images or tokens) to `debug.log` in its state directory (`~/.local/state/wingtip/` by
default).

## See also

- [`tui/README.md`](../../tui/README.md): the full reference.
- [`MT-ADMIN-QUICKSTART.md`](MT-ADMIN-QUICKSTART.md#tui-access-for-a-desktop-user): every `tui-access`
  option and exit status.
- [`GATEWAY_CHAT_API.md`](GATEWAY_CHAT_API.md): the gateway chat contract Wingtip uses.
- [`docs/proposals/WINGTIP_DESIGN.md`](../proposals/WINGTIP_DESIGN.md): the design, including the
  implemented protocol-1 socket hardening (M3, WI#458), planned client tokens (M2b), and the
  planned socket transport (M4).
