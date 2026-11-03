# LunarWing TUI

A terminal client for LunarWing with text, push-to-talk dictation and images. It runs on Bun with
OpenTUI 0.8.1 and React 19 and talks to a tenant's web gateway (HTTP plus SSE, bearer token). The
design is `/tmp/lw-tui-research/design/DESIGN.md` (sections 3, 5, 6, 7 and 9); this README covers
what you need in order to use it.

Status: milestones M1a (gateway client, LunarVoice, images, `tui-access`) and M1b (Hex voice sources).
Today's tenant daemons are served in **legacy mode** (header `legacy daemon`): everything works, but
replies are matched first in, first out, and generated images are not delivered. Daemons with the M2a
gateway contract are served in **exact mode** (header `exact`). The Unix-socket transport
(`--socket`, REPLv3) arrives with M4. Until then the client refuses it with a message.

## Contents

- [Requirements](#requirements)
- [Install](#install)
- [Getting access to a tenant (`tui-access`)](#getting-access-to-a-tenant-tui-access)
- [Launch](#launch)
- [Flags and environment](#flags-and-environment)
- [Configuration files](#configuration-files)
- [The screen](#the-screen)
- [Keys and the composer](#keys-and-the-composer)
- [Commands](#commands)
- [The info panel](#the-info-panel)
- [Capability matrix](#capability-matrix)
- [Voice](#voice)
- [Hex: local voice fallback (optional)](#hex-local-voice-fallback-optional)
- [Images](#images)
- [Privacy](#privacy)
- [Copying text](#copying-text)
- [Desktop notifications](#desktop-notifications)
- [Colour and motion (`NO_COLOR`)](#colour-and-motion-no_color)
- [Security behaviour](#security-behaviour)
- [Troubleshooting](#troubleshooting)
- [Known limitations](#known-limitations)
- [Development and tests](#development-and-tests)

## Requirements

- Bun 1.3.14 or newer (`engines: >=1.3.14`). Node is not supported, and the launcher refuses to run
  without Bun.
- Linux with a terminal that OpenTUI supports. kitty works best: it has the kitty keyboard protocol (key
  release events for push-to-talk) and the kitty image protocol. tmux works too, with the differences
  described under [push-to-talk environments](#push-to-talk-environments).
- Optional desktop tools, each used only by the feature that needs it. Without them the feature
  reports that it is unavailable:
  - `wl-paste`/`wl-copy` (Wayland clipboard fallback), or `xclip` (only without `WAYLAND_DISPLAY`)
  - `grim` and `slurp` (`/shot`)
  - `notify-send` (approval notifications)
  - `xdg-open` (`/image open`)
  - `parec` (recording fallback when OpenTUI audio is unavailable)
  - Hex 3.3.0 or newer (local transcription, see [Hex](#hex-local-voice-fallback-optional))

## Install

```bash
cd tui
bun install --frozen-lockfile
```

The dependencies are pinned exactly (`@opentui/core` and `@opentui/react` 0.8.1, `react` 19.3.1), and
the frozen lockfile keeps them that way. To check a checkout:

```bash
./node_modules/.bin/tsc --noEmit
bun test
```

## Getting access to a tenant (`tui-access`)

On a multi-tenant host the TUI runs as your desktop user (who owns the microphone, clipboard and Hex),
and that user cannot read tenant env files, sockets or tokens. An operator gives your user access to one
tenant's gateway with the mt-admin verb `tui-access`. It needs root, so Christopher runs it himself.
The TUI never calls it.

```bash
# Today's tenant daemons predate client tokens: copy the tenant's master token (prints a warning)
sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --for-user <user> --use-master-token

# Daemons with client-token support (M2b): a dedicated, revocable client token, optionally expiring
sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --for-user <user> --expires-in-days 90

# Registered client-token labels and dates (never tokens or digests)
sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --list

# Remove the access again
sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --for-user <user> --revoke
```

- A grant writes `~<user>/.config/lunarwing-tui/tenants/<tenant>.json` (mode 0600, owned by `<user>`,
  in 0700 directories) with the gateway URL, the tenant's uid and the token. Nothing prints the token.
- Without `--use-master-token` the grant registers only the token's SHA-256 with the tenant. A daemon
  that predates client tokens then rejects it, and the verb exits 2 with the advice to run
  `upgrade-tenant <tenant>` or to re-grant with `--use-master-token`.
- The master token carries full gateway API access and stays valid until it is rotated by hand. The
  TUI shows the sticky warning "this TUI uses the tenant master token" while it uses one.
- Running the grant again rotates the token. `--revoke` removes the tenant-side entry and the desktop
  file. A connected TUI then goes offline with a hint that names `tui-access`.
- Other options: `--url-host <host>` overrides the URL host, and `--no-verify` skips the post-grant
  probes. Exit status: 0 done, 1 refused, 2 files written but the running daemon rejected the token.
  The details are in `docs/guides/MT-ADMIN-QUICKSTART.md` ("TUI access for a desktop user").
- Never copy the output of `mt-admin tokens` into a file for the TUI. That verb is for the web login.

Keep `~/.config/lunarwing-tui/tenants/` out of dotfile sync and backups.

## Launch

```bash
tui/bin/lunarwing-tui --tenant <tenant>     # one tenant file from tui-access
tui/bin/lunarwing-tui                       # one tenant file: used directly; several: a picker
```

The launcher can be symlinked (for example into `~/.local/bin`). From inside `tui/`,
`bun run start -- --tenant <tenant>` does the same.

How the target is chosen (first match wins):

1. `--tenant`, a profile that names a tenant, or `--gateway-url`.
2. Exactly one valid tenant file: it is used.
3. Several: a picker lists tenant, URL, token kind and expiry, preselecting the last choice. Enter picks
   and Esc quits.
4. None: the TUI exits with status 2 and prints the `tui-access` command an operator can run.

A gateway that is not a tenant (a dev gateway) needs a URL and a token file:

```bash
tui/bin/lunarwing-tui --gateway-url http://127.0.0.1:8765 --token-file ~/.config/lunarwing-tui/tokens/dev.token
```

The token is **never** accepted on the command line: `--token`, `--gateway-token` and `--token=...`
exit with status 2, because argv is visible in `ps` and `/proc`. Use a 0600 token file or the
`LUNARWING_GATEWAY_TOKEN` variable, which the TUI reads once and then blanks in its own environment.

The gateway thread defaults to `--thread last`, a thread the TUI owns that is remembered per profile.
`new`, `assistant` or a thread UUID choose another. A second TUI on the same profile is a "second window":
it shows that label in the header, gets its own new thread unless `--thread` is given, and never
overwrites the remembered thread.

`/quit`, `/exit`, or Ctrl+C twice quits. Quitting stops every child process (`parec`, the Hex embedded
service) and removes the `/image open` copies.

## Flags and environment

```text
Connection:
  --tenant <name>                 tenant gateway from tenants/<name>.json (mt-admin tui-access)
  --profile <name>                profile from config.toml
  --config <path>                 config file (default: <config dir>/config.toml)
  --transport gateway|socket|auto
  --gateway-url <url>             non-tenant gateway (plain http only to loopback)
  --token-file <path>             0600 token file for a non-tenant gateway
  --socket <path>                 Unix socket of a same-uid daemon (M4; refused in this build)
  --thread last|new|assistant|<uuid>
Display:
  --image-protocol auto|kitty|sixel|blocks
Voice:
  --voice-order <id,...>          lunarvoice, hex-app, hex-embedded
  --no-voice
  --ptt-key <key>                 push-to-talk key, repeatable (default: f9, ctrl+space)
Diagnostics:
  --debug-log [<path>]            metadata-only 0600 debug log (default: <state dir>/debug.log)
  --help, --version
```

Environment: `LUNARWING_TUI_CONFIG_DIR` (the only config directory when set), `LUNARWING_TUI_CONFIG`,
`LUNARWING_TUI_PROFILE`, `LUNARWING_TUI_TRANSPORT`, `LUNARWING_GATEWAY_URL`,
`LUNARWING_GATEWAY_TOKEN_FILE`, `LUNARWING_GATEWAY_TOKEN`, `LUNARWING_TUI_IMAGE_PROTOCOL`,
`LUNARWING_TUI_DEBUG_LOG`, `LUNARWING_TUI_HEX_BIN` (the Hex binary), `HEX_APPLICATION_SUPPORT_DIR`,
`NO_COLOR`, and the usual `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR` and `XDG_DATA_HOME`.
Precedence everywhere: flags, then environment, then the profile in `config.toml`, then its global
tables, then the defaults.

## Configuration files

The config directory is the first one that exists of `$LUNARWING_TUI_CONFIG_DIR`,
`${XDG_CONFIG_HOME:-~/.config}/lunarwing-tui` and `~/.config/lunarwing-tui` (where `tui-access`
writes). It holds:

| Path | Rules |
|---|---|
| `config.toml` (optional) | owned by you and not group- or world-writable (it names commands); may be a symlink (dotfile managers); at most 256 KiB |
| `tenants/<tenant>.json` | written by `tui-access`; 0600, owned by you, regular file (no symlink), at most 4 KiB; schema `lunarwing-tui/tenant-access` version 1; `tenant` must equal the file name; `tenant_uid` required; an `expires_at` in the past is refused; `tenants/` must be yours and not group- or world-writable |
| `tokens/<profile>.token` | token files for non-tenant profiles: 0600, owned by you, regular file, at most 4 KiB, 16-512 printable ASCII characters |

Errors name the file and the failed check, never the content. Unknown keys in `config.toml` produce a
notice and are ignored. A value of the wrong type, or out of range, is an error that names the key.

```toml
default_profile = "derpy"
# transport = "gateway"            # top level: transport, thread, allow_insecure_http
[ui]
image_protocol = "auto"            # auto | kitty | sixel | blocks
max_turns_in_memory = 500          # 10-5000 transcript records kept
color = "auto"                     # auto | always | never; auto honours NO_COLOR
reduce_motion = false              # no spinner, VU meter at 4 Hz
cursor_blink = true
notify = "approvals"               # approvals | all | off
vision_verified_tenants = []       # tenants where the model is known to see images
[voice]
order = ["lunarvoice", "hex-app", "hex-embedded"]
ptt_keys = ["f9", "ctrl+space"]
repeat_delay_ms = 600              # your compositor's key repeat delay (Hyprland here: 300)
max_duration_s = 120               # hard cap 290
auto_stop_on_silence = false       # toggle mode only
silence_stop_ms = 3500
blur_policy = "hold"               # hold | always | never
# device = "C922"                  # substring of the capture device name
[voice.hex]
command = ["hex"]                  # argv, no shell
model = ""                         # empty: the first installed, verified English model
[profiles.derpy]
tenant = "derpy"                   # URL, token and tenant uid come from tenants/derpy.json
thread = "last"
[profiles.devgw]
gateway_url = "http://127.0.0.1:8765"
token_file = "~/.config/lunarwing-tui/tokens/devgw.token"
# expected_uid = 1000              # owner of the loopback listener; default: your uid
# allow_insecure_http = false      # plain http to a non-loopback host (sticky warning)
[profiles.devgw.ui]                # a profile may override [ui] and [voice]
notify = "off"
```

Without `--profile`, `--tenant <t>` uses the profile `<t>`. Otherwise `default_profile` applies when
no flag or `LUNARWING_GATEWAY_URL` names a target, then the picked or single tenant, then `default`.

Files the TUI writes (all 0600 in 0700 directories, written atomically):

| Path | Content |
|---|---|
| `${XDG_STATE_HOME:-~/.local/state}/lunarwing-tui/state.json` | the last picked tenant |
| `.../<profile>.json` | the remembered thread, the first-use acknowledgements for voice sources and image sources, the chosen capture device |
| `.../<profile>.lock` | pid of the primary instance of the profile |
| `.../debug.log` | only with `--debug-log`: event types, sizes, durations and status codes; never message text, transcripts, audio, image bytes, headers or tokens |
| `$XDG_RUNTIME_DIR/lunarwing-tui/` | `/image open` exports, removed on quit |

## The screen

From top to bottom: a one-line header, the transcript, the approval panel (only while a tool call waits
for approval), up to two notices, the attachment bar (only with pending images), the composer with the
mic widget on its left, and a one-line footer that lists the keys for the current state. F1 opens the
help overlay.

The header names the tenant or profile, the connection state (`connecting`, `ready`, `reconnecting in
N s`, `OFFLINE`), the engine, `exact` or `legacy daemon`, the image protocol, and `second window` when
it applies. Every state carries a text label, so nothing depends on colour.

## Keys and the composer

| Keys | When | Action |
|---|---|---|
| Enter | always | send (queued while a turn runs); confirm a guarded send or a typed approval; send the credential |
| Ctrl+J, Alt+Enter, Shift+Enter | composer | new line (Shift+Enter needs the kitty keyboard protocol) |
| Ctrl+S, Ctrl+Enter | always | interrupt the running turn, then send; with an empty message, interrupt only |
| F9, Ctrl+Space | voice | push-to-talk (tap to start and stop, or hold) |
| F1 | always | help |
| Esc | always | close the panel; cancel a confirmation; cancel dictation; dismiss the newest notice |
| Ctrl+V | composer | paste from the clipboard (image, copied files or text) |
| Ctrl+R / Ctrl+X | images attached | remove the last / every attachment |
| Alt+Y / Alt+A / Alt+N | approval pending | approve / approve and always allow this tool / deny |
| Alt+V | approval pending | view the full approval text |
| Alt+], Alt+J | several approvals | focus the next pending approval; inside tmux use **Alt+J** (tmux sends Alt+] in the legacy encoding as `ESC ]`, which is dropped) |
| Alt+R | voice clip kept | retry the kept voice clip |
| Up, Down | first / last line | previous / next sent message or cleared draft |
| Ctrl+_ (what Ctrl+- sends) / Ctrl+. | composer | undo / redo, also inside tmux |
| PageUp, PageDown | always | scroll the panel or the transcript; scrolling to the end follows new text again |
| Ctrl+C | always | cancel dictation; else clear the message (Ctrl+_ restores it); else press twice within 2 s to quit |

The composer wraps words and grows from one to six rows. A paste keeps its line breaks. A byte counter
appears above 80 % of the daemon's message limit. A message typed while a turn runs is queued. On
legacy daemons each queued message still gets its own reply in order.

Approvals:

- An approve key acts only on the prompt that is on screen, and only after it has been on screen for at
  least 1 s. A slash command also requires the prompt to have existed before you started typing, so a
  prompt that pops up mid-keystroke is never approved by accident ("approval just arrived; press
  again"). While an info panel (help, `/voice`, ...) covers the approval panel nothing can be answered;
  after Esc the 1 s starts again. In the full view (Alt+V) the keys answer the prompt shown there, Alt+J
  shows the next one in full, and a prompt that was answered meanwhile offers no keys ("no longer
  pending - Esc closes this view"). The `2/3` counter follows the arrival order.
- When the parameters do not fit the panel, the panel shows `[+N lines hidden - Alt+V view all]`, and
  approving is refused until you have opened the full view ("Alt+V: open the full parameters first").
  Deny always works. Invisible characters in approval text show as visible `\u{...}` escapes. Widths are
  measured the way OpenTUI draws the text (wide symbols such as hexagrams, emoji sequences), so a
  parameter line is wrapped, never cut at the panel's edge. On narrow terminals the action row drops
  `[Alt+A]`, then `[Alt+Y]`, then `[Alt+N]` before it would drop the Alt+V button, and the footer
  starts with "Alt+V view" while text is hidden.
- A typed or dictated "yes", "no", "okay" and similar words, sent while a prompt is pending, ask for a
  confirming second Enter and then answer the focused prompt. "Yes, but use staging" is sent as text.
  Typed text never means "always".
- With nothing pending, a bare keyword such as `yes` needs a second Enter within 10 s, because the
  daemon may take it as an approval answer.

Credential requests (`auth_required`): the composer switches to a masked field
(`credential for <extension>: **** (N chars)`). Enter sends the credential to the gateway's
`auth-token` route, never as a chat message. Esc arms, and a second Esc within 2 s cancels. The secret
never appears on screen, in history or in the debug log. A V2 authentication gate that is still pending
when the TUI starts, reconnects or resyncs (`GET /api/chat/pending-gates`) puts the composer back into
credential mode, because the daemon would take the next message on the thread as the credential. Such a
gate-backed credential mode does not end after 290 s; it ends with the gate (`gate_resolved`), on Esc Esc,
or when the credential is sent.

## Commands

| Command | Action |
|---|---|
| `/help` | the help overlay (`//help` asks the daemon) |
| `/img <path>`, `/attach <path>` | attach an image file (`~/`, quotes and `file://` URIs accepted) |
| `/shot [full]` | attach a screenshot of a region (or of one output) |
| `/image open [n]`, `/image save [n] [path]` | open in the desktop viewer, or save as a 0600 file, the newest (n-th newest) generated image |
| `/copy [n]` | copy the newest (n-th newest) reply |
| `/voice [status\|use <id>\|auto\|order <ids>\|device [name]\|test\|privacy\|retry]` | voice sources, devices and the privacy panel |
| `/transport [status\|reconnect]` | connection details; reconnect now |
| `/interrupt`, `/stop`, `/steer <note>` | stop the running turn; add a note to it |
| `/approve`, `/always`, `/deny`, `/approval view` | answer or view the focused approval |
| `/thread [list\|new\|<id prefix>]` | the threads of this gateway; switch threads |
| `/clear`, `/new` | forwarded to the daemon |
| `//<text>` | send `/<text>` to the daemon |
| `/quit`, `/exit` | quit the TUI |

`/quit`, `/exit` and `/shutdown` are never sent to the daemon, on any transport, because on today's
daemons they would stop it. `/shutdown` and `/restart` are refused. To ask the daemon itself, write
`//restart`, which also needs a second Enter. Other unknown `/commands` are forwarded unchanged.

## The info panel

The info panel is an overlay. PageUp and PageDown scroll it, and Esc closes it before Esc does anything
else.

- **Help**: F1 or `/help`. It shows the keys, the push-to-talk hint for your environment, and the commands.
- **Voice**: `/voice` or `/voice status`. Every source with its availability and reason, the
  recorder, the device, the longest recording, and the kept clip.
- **Privacy**: `/voice privacy`. The [privacy table](#privacy), with the active backend marked.
- **Devices**: `/voice device`. Capture devices; `/voice device <name>` stores a choice for the
  profile, and `/voice device ""` returns to the default source.
- **Transport**: `/transport`. URL, listener-owner check, token source (never the token), capabilities,
  limits, image protocol and warnings.
- **Threads**: `/thread list`.
- **Approval**: Alt+V or `/approval view`. The full tool description and parameters. Alt+Y, Alt+A and
  Alt+N work inside it.

## Capability matrix

The client asks the gateway for `GET /api/chat/capabilities`. A 404 means a legacy daemon, and the
client degrades instead of failing.

| Capability | Exact mode (M2a daemons) | Legacy mode (today's daemons) | Socket (M4) |
|---|---|---|---|
| Text turns, queueing, interrupt, steer | yes, matched by id | yes, matched first in, first out | M4 |
| Token streaming | when the engine streams | when the engine streams (detected from the first chunk) | no (V1 on the socket) |
| Activity lines (tools, thinking) | yes | yes | REPLv3 only |
| Images you send | yes | yes (the client validates) | REPLv3 only |
| Generated images | yes | no (`/transport` says so; a turn that ran `image_generate` gets a note) | REPLv3 only |
| Approvals with a panel | yes | yes | REPLv3 only |
| Threads and history | yes | yes | REPLv3: own thread token |
| LunarVoice transcription | when configured | when `/api/voice/status` says configured | REPLv3 only |
| Exact turn end | `turn_finished` events | inferred: the reply, status hints, and a history resync 10 s after a `Done` without a reply; the oldest waiting turn is marked `unanswered` after 15 s without any event | REPLv3 only |

Limits (images, message size, transcription size) come from the daemon. Legacy gateways get the
defaults: 100,000-byte messages, 5 images of at most 5 MiB and 7 MiB in total, PNG, JPEG, GIF and WebP,
an 18 MiB WAV for transcription, and 30 messages and 10 transcriptions per minute.

## Voice

Dictation inserts text into the composer at the cursor. You read and edit it, then send with Enter.
Nothing is sent by voice alone. The TUI always does the recording itself: OpenTUI's audio capture at
16 kHz mono, or `parec` when OpenTUI audio is unavailable. Audio stays in memory and is zero-filled
afterwards.

- **Sources**, tried in `[voice] order` (default `lunarvoice`, `hex-app`, `hex-embedded`):
  - `lunarvoice`: the daemon's LunarVoice sidecar through the gateway
    (`POST /api/voice/transcribe`), on the daemon's host.
  - `hex-app` and `hex-embedded`: Hex on this machine (see below).

  `/voice use <id>` pins one source for the session, and `/voice auto` restores the order.
- **Fallback per clip**: a busy source is retried after 1, 2 and 4 s; unavailable, rate-limited,
  timed-out, processing, model-not-ready, too-large and auth failures move on to the next source with
  the same recording; "no speech", a refused request and an unsupported audio format stop. A success after a fallback names the
  source and why ("transcribed by hex-app (lunarvoice: busy)").
- **Kept clip**: when every source failed, the last recording is kept in memory for one retry: Alt+R
  or `/voice retry` resends it, and Esc discards it. It is dropped after 10 minutes, or when a new
  recording starts. The notice "voice failed (Alt+R retry, Esc discard): <every attempt>" stays while
  the clip is kept.
- **Local checks**: a clip under 0.3 s is discarded as too short, and a clip whose peak stayed below
  -50 dBFS is discarded with "no signal from <device> - /voice device". Recordings stop at
  `max_duration_s` (default 120 s, cap 290 s), with a warning 10 s before.
- **Devices**: the device stored with `/voice device <name>`, then `[voice] device`, then the default
  source.
- `--no-voice` creates no audio engine and probes no voice source. Dictation is also disabled while a
  credential is being entered.

### Push-to-talk environments

| Environment | Hold to talk (release stops) | Tap to start, tap to stop | Notes |
|---|---|---|---|
| kitty direct (kitty keyboard protocol) | yes, real key release events | yes | releasing Ctrl before Space still ends a Ctrl+Space hold |
| kitty inside tmux | inferred from key repeat (hold F9; recording stops about 250 ms after the repeats stop) | yes | tmux never forwards key releases; Ctrl+Space arrives as NUL and works |
| other terminals without the protocol | inferred from key repeat | yes | |
| any terminal with the mouse | hold the mic widget, release anywhere | click toggles | inside tmux the press and release are forwarded |

A short tap (under 300 ms) toggles recording, and a longer press is push-to-talk. Set
`[voice] repeat_delay_ms` to your compositor's key-repeat delay so held keys are recognised quickly.
When the terminal loses focus during a hold, the recording stops (`blur_policy = "hold"`; `always` also
stops toggle recordings; `never` keeps everything running). The mic widget shows `( mic )`, `( ... )`
while opening, `REC 0:07` with a level meter while recording, `[ .... ]` while transcribing, and
`(no mic)` when nothing can transcribe. `/voice test` explains how to watch the level meter.

## Hex: local voice fallback (optional)

When LunarVoice cannot transcribe (busy, offline or not configured), the TUI can use
[Hex](https://github.com/anomalyco/hex) on this machine. Hex is optional. Without it, `/voice` lists
`hex-app` ("not running") and `hex-embedded` ("not installed") as unavailable with their reasons, and
LunarVoice dictation works as before. The TUI always does the recording. Hex only transcribes the
in-memory WAV through its local API (`POST /transcriptions`), and audio is never written to disk. Hex's
own dictation (`/dictations`) and the Hex CLI are not used in this release.

- **`hex-app`** uses the running Hex app. The TUI reads
  `${HEX_APPLICATION_SUPPORT_DIR:-${XDG_DATA_HOME:-~/.local/share}/hex}/local-api.json`. That file must
  be a regular file (not a symlink), owned by you, mode 0600, at most 4 KiB, and must hold
  `{port, token, apiVersion: "2", pid}` with a `hex_<64 hex>` token. The pid must be alive and yours.
  Before the token is sent anywhere, the TUI checks in `/proc/net/tcp` that your uid owns the
  `127.0.0.1:<port>` listener, then calls `GET /health` (300 ms). It checks the file every 5 s and
  probes again when Hex starts or stops.
- **`hex-embedded`** is for when no Hex app is running. At the first transcription the TUI starts
  `hex service --embedded` itself. This needs Hex 3.3.0 or later, checked with `hex --version`; the
  binary is `[voice.hex] command` or `LUNARWING_TUI_HEX_BIN`. The child gets an allowlisted environment
  without `TRANSCRIBE_DUMP_DIR`. It must print one ready line within 15 s
  (`{"type":"ready","url":"http://127.0.0.1:<port>","token":"hex_...","apiVersion":"2","pid":<pid>}`;
  the pid must be the child's own), and the TUI checks the port's owner again. The child lives only
  while its stdin stays open, and the TUI never writes to it. If the TUI quits or crashes, even by
  SIGKILL, the child exits within about 5 s. It stays up 10 minutes after the last transcription. On
  quit the TUI closes stdin, sends SIGTERM after 6 s and SIGKILL after 10 s. Embedded mode writes no
  discovery file. Hex writes `logs/process.log` and a temporary `logs/embedded-<pid>.ndjson`, without
  transcript text. While the Hex app runs, `hex-embedded` stays idle ("the Hex app is running (hex-app
  transcribes)"), so two copies of a model are never loaded.
- **Models**: the TUI never downloads a model (it never calls `POST /models/{id}/prepare`). It uses
  `[voice.hex] model` if set. Otherwise it uses the first installed and verified model that supports
  English from `GET /models`. Without one, the source is unavailable with the hint
  `hex model install parakeet_unified_en --language en` (about 731 MB). A configured model that Hex does
  not offer, or that does not support English, makes the source unavailable, so a configuration slip
  never throws a recording away.
- **Errors**: a full Hex queue counts as busy (retried after 1, 2 and 4 s). A 401 makes the TUI re-read
  the discovery file once (`hex-app`) or restart the embedded child once (`hex-embedded`). Every Hex
  request refuses to run while an HTTP(S) proxy variable is set, so the Hex token never reaches a proxy.
- **Tested version**: none yet. The automated tests use a mock that follows Hex dev `54a3a66` (Cargo
  3.3.0). Record the version here after the supervised test 16.9 against the released
  `hex-3.3.0-linux-x64` asset: verify the binary or RPM checksum against the GitHub asset digest, then
  `hex model install parakeet_unified_en --language en`, `/voice use hex-embedded`, and `/voice use
  hex-app` with the app running.

## Images

Ways to attach (the image is sent with your next Enter):

- **Ctrl+V**: the clipboard through OpenTUI's host clipboard, falling back to `wl-paste`. Copied files
  (a `text/uri-list`) are attached as files, and text is inserted at the cursor. The clipboard is read
  only on Ctrl+V.
- **`/img <path>`, `/attach <path>`**: a file. Relative paths resolve against the directory you
  started the TUI from.
- **Drag and drop** from a file manager: kitty delivers the paths as a paste, and tmux forwards it.
  Pastes that are only image paths are attached; anything else is inserted as text. kitty's paths are
  unquoted, so a name such as `Chris's shot.png` is taken as it is; WezTerm's backslash escapes and
  VTE's single quotes are understood too. A paste of image paths that cannot be attached (more than the
  per-message limit) is inserted as text with a notice saying so.
- **`/shot`**: a region selected with `slurp`, captured with `grim -s 1`. **`/shot full`**: click one
  output. A PNG over the limit is retried once as JPEG (quality 85). `/shot` is recommended over the
  clipboard, because clipboard managers such as cliphist keep a copy of every clipboard image.

Pending images show in the attachment bar: previews when the terminal has 16 rows or more, one line
below that. Ctrl+R removes the last image and Ctrl+X removes every image.

Limits and validation:

- Only PNG, JPEG, GIF and WebP, recognised by their magic bytes. File extensions, clipboard types and
  data-URL prefixes are not trusted, and BMP, SVG, TIFF, HEIC, AVIF and ICO are refused. The detected
  type is the one sent.
- The per-image, count and total limits come from the daemon (legacy gateways: 5 images, 5 MiB each,
  7 MiB together). The whole request must also fit the gateway's body limit after base64 encoding. The
  limits are checked when you attach and again when you send.
- The image must decode with at least 1x1 pixels. OpenTUI 0.8.1 refuses images above 25 megapixels or
  16384 pixels on a side, so the client refuses those too (a 6000x6000 photo, for example), although
  the daemon would accept them. There is no client-side downscaling. An oversize image is refused
  with "select a smaller region (/shot) or compress it".

Generated images (exact mode only) are rendered under the reply from validated bytes (at most 56x12
cells), never from a path or URL the server names. `--image-protocol auto|kitty|sixel|blocks` (or
`[ui] image_protocol`) selects the renderer. Inside tmux, `auto` uses block characters, and forcing
`kitty` there misplaces images (a known OpenTUI issue; `/transport` warns). `/image open [n]` writes the
image to `$XDG_RUNTIME_DIR/lunarwing-tui/` (0600) and opens it with `xdg-open`, and those copies are
removed on quit. `/image save [n] [path]` writes a new 0600 file and never overwrites one. Only the
newest 20 images keep their bytes in memory; older ones become placeholders ("image no longer in
memory").

### Where the images you send are stored

**Every image you send from the TUI (clipboard, `/img`, drag and drop, `/shot`) is stored by the daemon
as base64 under `attachments/<channel>/<id>/` in the tenant's workspace, with no expiry.** It is
searchable and readable afterwards from every channel and from the web UI (`attachment_store`,
`memory_read`). On multi-tenant hosts that is the tenant uid's storage, readable by that tenant's
agent and tools. The first image from each source shows the notice "images you send are stored in
<tenant>'s workspace (no expiry) and are readable from every channel", and the attachment bar's caption
ends with "stored in <tenant>'s workspace".

Whether TUI image turns should skip this storage is decision **DEC-3**, which is still open. Storing and
disclosing is the current default, because on V1 engines an image is visible to the model only in its
own turn, and the stored copy is how later turns refer back to it.

### Vision caveat

Images are delivered to the tenant's configured model, which may ignore them. On V1 engines an image
reaches the model only in the turn it is sent with; V2 carries images as transient parts. There is no
vision capability check, and LunarVision enrichment applies to XMPP only. The default TensorZero
`lunarwing` function routes about 99 % of traffic to Kimi K3, and whether that model accepts images is
not yet verified (supervised test 16.5). Until it is, the first image you send in a session shows
"images go to the configured model; vision is not verified for <tenant>" (also shown in `/transport`).
List tenants where it is verified in `[ui] vision_verified_tenants` to drop the notice.

## Privacy

| Path | What persists |
|---|---|
| TUI recorder | nothing on disk. PCM and WAV exist only in memory and are zero-filled after success, after a cancel, or when the kept clip is dropped (at most one failed clip, for at most 10 min). No temp files; the capture stream is opened per recording |
| LunarVoice (gateway) | no daemon persistence (the preview path never touches the attachment store). The sidecar keeps a 10-minute in-memory cache keyed by the audio hash. Audio stays on the daemon's host (loopback sidecar) |
| Hex `/transcriptions` (app or embedded) | no WAV, no history, no transcript in event logs. `process.log` is written. The TUI removes Hex's native dump flag (`TRANSCRIBE_DUMP_DIR`) from the child environment |
| Images | every image you send is stored in the tenant's workspace with no expiry and is readable from every channel and the web UI (see [above](#where-the-images-you-send-are-stored); decision DEC-3) |
| Clipboard | cliphist (or another clipboard manager) records image and text copies, including the TUI's own copies. The TUI cannot prevent that |
| Hex app dictation (`/dictations`, CLI) | not used in this release (it would keep Hex history for a week and `live.ndjson`) |

The first use of each voice backend and of each image source shows a one-line notice with its privacy
note, once per profile. Transcripts never touch the clipboard, and the debug log never records
transcripts, audio or message text.

## Copying text

Drag over transcript text with the mouse: when you release, the selection is copied ("copied N
characters (cliphist records copies)"). The TUI writes through OpenTUI's host clipboard, falls back to
`wl-copy` under Wayland, and otherwise uses OSC 52 (outside tmux only, because tmux ignores
application OSC 52 with `set-clipboard external`). `/copy` copies the newest reply and `/copy <n>` the
n-th newest. Headers, footers, buttons, panel frames and captions are not selectable.

In kitty, **Shift+drag** uses kitty's own selection instead of the TUI's, which is handy for copying
across the whole window.

## Desktop notifications

When the terminal does not have focus and an approval arrives, the TUI runs
`notify-send -a lunarwing-tui -- "LunarWing <tenant>" "approval needed: <tool>"`, at most once per 10 s.
Only the tool name is sent, never the description or the parameters. `[ui] notify = "all"` also notifies
when a turn finishes while unfocused, and `"off"` silences it. Without `notify-send` nothing happens.
Terminal bells are not used.

## Colour and motion (`NO_COLOR`)

`[ui] color = "auto"` turns colour off when `NO_COLOR` is set and non-empty; `never` always turns it
off, and `always` ignores `NO_COLOR`. The monochrome theme draws all text in the terminal's own default
foreground (so it works on light and dark terminal themes) and uses only bold, inverse and underline, and
every state keeps its text label. `[ui] reduce_motion = true` stops the spinner and redraws the level
meter at 4 Hz. `[ui] cursor_blink = false` stops the cursor blinking.

## Security behaviour

- Before any token leaves the client for a loopback port (the gateway, or Hex), the TUI checks in
  `/proc/net/tcp` and `/proc/net/tcp6` that the listening socket belongs to the expected uid: the
  tenant's uid from the tenant file, `expected_uid`, or your own. A mismatch goes offline with "port N
  is held by uid U, expected X" before any authenticated request.
- Plain `http` is accepted only to loopback (127.0.0.0/8, `::1`, `localhost`), unless
  `allow_insecure_http = true` (sticky warning). URLs with credentials, a query or a fragment are refused,
  and so are redirects.
- Tokens never appear on argv, in URLs, in logs, in error messages or on screen. HTTP proxy variables are
  neutralized at start. If one is still set, gateway and Hex requests are refused rather than sent
  through the proxy.
- Child processes (`parec`, `wl-paste`, `wl-copy`, `grim`, `slurp`, `notify-send`, `xdg-open`, `hex`) are
  started without a shell, with an argv array and an allowlisted environment. They never get a token,
  the proxy variables or `TRANSCRIBE_DUMP_DIR`.
- Server text (replies, activity, notices, captions) is stripped of escape sequences, control characters
  and bidi controls before it is shown. Approval text shows them as visible escapes instead.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| exits with "no tenant access is configured for <user>" | no valid `tenants/*.json`: ask for `tui-access` (above). Invalid files are listed with their reason |
| a tenant file is refused (mode, owner, expired, missing `tenant_uid`) | re-run `tui-access` for your user; do not edit the file by hand |
| tenant files from `tui-access` are not found | `${XDG_CONFIG_HOME}/lunarwing-tui` exists and is searched first; move the files there, or set `LUNARWING_TUI_CONFIG_DIR=~/.config/lunarwing-tui` |
| `OFFLINE` with "the gateway rejected the token (HTTP 401 ...)" | client token on a daemon without client tokens: run `upgrade-tenant <t>` or re-grant with `--use-master-token`; master token changed: re-grant with `--use-master-token`; token file: replace the file |
| `OFFLINE`, "... was refused (HTTP 403) ...; client tokens reach only chat, voice and status routes" | a client token reached a route outside its scope |
| `OFFLINE`, "port N is held by uid U, expected X" | another account owns the port. Check the tenant (`mt-admin status <t>`) and the `tenant_uid` in your tenant file |
| "HTTP_PROXY is set: gateway requests are never sent through a proxy" | unset the proxy variables for the TUI, or set `NO_PROXY='*'` |
| `reconnecting in N s` | the gateway is down or restarting; the TUI keeps retrying (backoff up to 15 s). `/transport reconnect` retries at once |
| header says `legacy daemon` | expected with today's daemons; see the [capability matrix](#capability-matrix) |
| `(no mic)` | no source can transcribe: `/voice` lists the reason per source (gateway voice not configured, Hex not running or not installed). The footer and the welcome then offer no push-to-talk key |
| "voice failed (Alt+R retry, Esc discard): ..." | every source failed; the clip is kept for 10 min |
| "no signal from <device> - /voice device" | the capture device is muted or wrong; pick another with `/voice device` |
| "this daemon does not accept images" | the transport has no image support (legacy socket, M4) |
| "image generated, but this daemon does not deliver images to clients" | legacy daemon (needs M2a) |
| Alt+] does nothing inside tmux | use Alt+J (the footer offers Alt+J wherever Alt+] cannot arrive) |
| "the socket transport is not in this build yet (M4)" | connect through a gateway (`--tenant` or `--gateway-url`) |
| `second window` in the header | another TUI already runs on this profile; this one uses its own thread |

`--debug-log` writes a metadata-only log (0600) to `<state dir>/debug.log` or to the given path.

## Known limitations

- Legacy daemons (today's tenants): replies are matched first in, first out; a turn that gets no event
  for 15 s is marked `unanswered`, and a finished turn without a reply is resolved by a history resync;
  generated images are not delivered.
- On V1 gateways, after a credential is accepted through `auth-token`, the daemon leaves credential mode
  only on the session's active thread. A TUI thread that is not the active thread may consume its next
  message as a credential (the text is not leaked, but that message is lost).
- On V2 gateways, credential mode ends on `gate_resolved`, on Esc Esc, or when the credential is sent. A
  credential completed elsewhere (for example in the web UI) without a `gate_resolved` leaves the TUI
  in credential mode until Esc Esc.
- Copy on select copies any finished drag selection, including one inside the composer.
- Images above 25 megapixels are refused (OpenTUI 0.8.1 limit, no downscaling).
- The microphone, clipboard, screenshot, notification and Hex paths are tested with fakes. The
  supervised live tests of design section 16 are still open.

## Development and tests

```bash
cd tui
bun install --frozen-lockfile
./node_modules/.bin/tsc --noEmit
bun test                                            # everything
bun test test/integration/gateway-transport.test.ts # one file
```

- Layout: `src/` (config, state, controller, transport, voice, images, components), `test/` (unit,
  integration and OpenTUI `testRender` UI tests, mocks and fakes). The test preload (`test/setup.ts`)
  fails the run on unhandled rejections, removes temp directories, refuses to spawn real desktop tools,
  `hex`, `sudo`, `systemctl` or `rc-service`, and refuses non-loopback fetches. Tests inject clocks
  (`ManualClock`) and child processes (`test/helpers/fake-spawn.ts`), and every mock listens on
  `127.0.0.1:0`.
- A mock gateway for manual runs:
  `bun test/mocks/mock-gateway.ts --mode legacy|current [--token-file <0600 file>]` prints
  `LISTENING <port>` and answers every message with `echo: <text>`. Then run
  `bin/lunarwing-tui --gateway-url http://127.0.0.1:<port> --token-file <same file> --no-voice`.
- CI: the `tui-ts` job in `.gitlab/ci/validate.yml` runs `bun install --frozen-lockfile`,
  `./node_modules/.bin/tsc --noEmit` and `bun test` in `tui/` on Bun 1.4.4. The script is pinned
  line for line by `.gitlab/scripts/validate_config.py` (`WORKER_TS_LANE_SCRIPTS`).
- The tests must pass on the host's Bun 1.3.14 and on CI's 1.4.4 (which runs as root): do not depend
  on file-permission denials that root bypasses, or on runtime-specific error text.
