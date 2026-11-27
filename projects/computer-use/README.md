# LunarWing computer use for Wayland desktops

A stdio MCP server that lets LunarWing see and operate an **existing**
Wayland desktop session: screenshots of one monitor at a time, pointer
movement, clicks, drags, scrolling, key chords and text entry. It runs as the
desktop user, either started directly by LunarWing or over SSH. It opens no
network listener, starts no relay and uploads nothing to third parties (the
only outbound connection is the optional, operator-configured LunarVision
endpoint described below).

Status: **not yet tested against a real compositor.** The automated suite
drives every layer (MCP framing, frame guard, coordinate mapping, the Wayland
wire protocol, keymap generation, grim invocation, the portal D-Bus sequence)
against fakes: a fake compositor on a socket pair, a fake `grim` runner and
fake D-Bus/GStreamer objects. The opcode tables were checked against the
protocol XML and against Hyprland 0.57.2's generated interface tables, and the
generated keymaps compile with libxkbcommon 1.13, but no input has been sent
to a live session. Run the acceptance checklist at the end before relying on it.

## Supported desktops

| Backend | Compositors | Input | Screenshots | Consent |
|---|---|---|---|---|
| `wlr` | Hyprland, Sway, labwc, river, Wayfire, niri (and other compositors with the same protocols) | `zwlr_virtual_pointer_v1` and `zwp_virtual_keyboard_v1`, spoken by the connector's own small Wayland client | `grim` (wlr-screencopy / ext-image-copy-capture) | None from the compositor by default (see Hyprland permissions below); LunarWing's tool approval is the gate |
| `portal` | GNOME, KDE Plasma, COSMIC (portal 1.7.0 or later) | XDG RemoteDesktop portal `Notify*` methods | XDG ScreenCast portal + PipeWire via GStreamer `pipewiresrc` | The desktop's sharing dialog on `desktop_connect` |

`--backend auto` (the default) probes, in this order: if the compositor's
registry offers both `zwlr_virtual_pointer_manager_v1` and
`zwp_virtual_keyboard_manager_v1` it uses `wlr`; otherwise, if PyGObject is
importable and the session bus offers `org.freedesktop.portal.RemoteDesktop`,
it uses `portal`; otherwise `desktop_connect` fails with a message listing
what was probed. Both probes are read-only (a registry listing and one D-Bus
property read). `desktop_status` shows the same report.

COSMIC implements the virtual keyboard but not the virtual pointer, so it
uses the portal. Mutter (GNOME), KWin (KDE) and Weston offer neither wlroots
protocol. Hyprland and xdg-desktop-portal-wlr do not offer the RemoteDesktop
portal.

## Requirements

Python 3.10 or newer. The core and the `wlr` backend use the standard library
only. PyGObject is imported only by the `portal` backend, when it is used.

| Purpose | Fedora | Arch | Gentoo |
|---|---|---|---|
| wlr screenshots | `grim` | `grim` | `gui-apps/grim` |
| Portal frontend | `xdg-desktop-portal` | `xdg-desktop-portal` | `sys-apps/xdg-desktop-portal` |
| GNOME portal backend | `xdg-desktop-portal-gnome` | `xdg-desktop-portal-gnome` | `sys-apps/xdg-desktop-portal-gnome` |
| KDE portal backend | `xdg-desktop-portal-kde` | `xdg-desktop-portal-kde` | `kde-plasma/xdg-desktop-portal-kde` |
| COSMIC portal backend | `xdg-desktop-portal-cosmic` | `xdg-desktop-portal-cosmic` | not in ::gentoo |
| PipeWire | `pipewire`, `wireplumber` | `pipewire`, `wireplumber` | `media-video/pipewire`, `media-video/wireplumber` |
| GStreamer `pipewiresrc` | `pipewire-gstreamer` | `gst-plugin-pipewire` | `media-video/pipewire` with `USE=gstreamer` |
| GStreamer convert/scale/crop/encode | `gstreamer1-plugins-base`, `gstreamer1-plugins-good` | `gst-plugins-base`, `gst-plugins-good` | `media-libs/gst-plugins-base`, `media-libs/gst-plugins-good` |
| PyGObject (portal only) | `python3-gobject` | `python-gobject` | `dev-python/pygobject` |

On Gentoo, Sway, labwc and Wayfire are in ::gentoo; river and niri are in the
GURU overlay; Hyprland comes from a third-party overlay. On OpenRC the portal
and PipeWire must be started by the session or as OpenRC user services; the
`wlr` backend needs neither.

Check the portal backend's GStreamer pieces without opening a session:

```bash
python3 -c 'import gi; gi.require_version("Gst", "1.0"); from gi.repository import Gst; Gst.init(None); print({n: Gst.ElementFactory.find(n) is not None for n in ("pipewiresrc", "videoconvert", "videoscale", "videocrop", "pngenc", "jpegenc", "appsink")})'
```

## How the session is found

LunarWing starts stdio servers with a scrubbed environment: `XDG_RUNTIME_DIR`,
`HOME` and `PATH` pass through; `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`
and `DISPLAY` do not unless set with `--env`. The connector therefore falls
back as follows:

- Wayland socket: `WAYLAND_DISPLAY` (a name in the runtime directory or an
  absolute path). If it is unset, the single socket named exactly
  `wayland-N` in `$XDG_RUNTIME_DIR` (or `/run/user/<uid>`) is used. Lock files
  and helper sockets such as `wayland-1-swww-daemon..sock` are ignored. Zero
  or several candidates is an error; pass `--env WAYLAND_DISPLAY=wayland-1`.
- Session bus (portal only): `DBUS_SESSION_BUS_ADDRESS`, else
  `unix:path=$XDG_RUNTIME_DIR/bus` if that socket exists. Hosts whose session
  bus lives elsewhere (common with OpenRC and `dbus-launch`) must pass the
  address with `--env` or in the SSH launcher. Do not wrap the connector in
  `dbus-run-session`, which creates a different bus.

## Registering with LunarWing

The examples name the server `computer`, so LunarWing shows the tools as
`computer_desktop_screenshot` and so on. `--require-approval` takes the raw
MCP tool name (without the server prefix) and can be repeated. A listed tool
asks for approval on every call in interactive turns and never runs in
routines, background jobs or missions, where nobody can approve it.
`--interactive-only` keeps the tools out of routines, background jobs,
missions and shared conversations (group chats and rooms): they run only in a
private turn you are watching. Both flags are recommended for desktop control;
`lunarwing mcp update` keeps them, but re-running `lunarwing mcp add` for the
same name replaces the entry, so repeat them there.

How LunarWing shows the screenshots: each `image` block is validated and
attached to a user-role message right after the tool results, for the model
only. The transcript and database keep the text result alone. Within one turn
only the newest three screenshot steps (at most 8 MiB) are resent, so take a
fresh screenshot whenever you need to see the screen again. Shared
conversations never get the images. For a model without vision, set
`TOOL_RESULT_IMAGES=false` in the daemon's environment and use
`desktop_observe` (below) instead.

### Same user (LunarWing runs as the desktop user)

```bash
lunarwing mcp add computer --transport stdio \
  --command /usr/bin/python3 \
  --arg=-I --arg=/absolute/path/lunarwing2/projects/computer-use/computer-use.py \
  --env WAYLAND_DISPLAY=wayland-1 \
  --interactive-only \
  --require-approval desktop_type_text \
  --require-approval desktop_key \
  --require-approval desktop_click \
  --require-approval desktop_drag
```

Add `--env DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/<uid>/bus` for the
portal backend if the bus is not at the default path. Connector options go
after the script, for example `--arg=--backend --arg=wlr --arg=--max-edge
--arg=1280`. Test the registration with `lunarwing mcp test computer`, then
activate it through the extension UI or `tool_activate`.

### Another user over SSH (multi-tenant hosts)

Multi-tenant deployments run each daemon as its own service user without a
graphical session, so a connector started directly by a tenant daemon would
look for the tenant's own (nonexistent) session. Run the connector as the
desktop user over SSH instead.

On the desktop machine, create an executable launcher, for example
`/home/desktop-user/.local/bin/lunarwing-computer`:

```sh
#!/bin/sh
exec /usr/bin/python3 -I /home/desktop-user/lunarwing2/projects/computer-use/computer-use.py --backend auto
```

It must print nothing to stdout: stdout carries newline-delimited MCP JSON
only. Logs go to stderr. Shell profiles that print on login break the stream.

Give the LunarWing tenant a dedicated SSH key and restrict it on the desktop
side with a forced command in `~/.ssh/authorized_keys`, so the key can run
only the connector:

```
restrict,command="/home/desktop-user/.local/bin/lunarwing-computer" ssh-ed25519 AAAA... lunarwing-computer
```

On the LunarWing host, configure an SSH alias (`my-desktop`) with the desktop
user, that identity file and a verified known-hosts entry, then:

```bash
lunarwing mcp add computer --transport stdio \
  --command /usr/bin/ssh \
  --arg=-T --arg=-o --arg=BatchMode=yes \
  --arg=-o --arg=StrictHostKeyChecking=yes \
  --arg=my-desktop --arg=/home/desktop-user/.local/bin/lunarwing-computer \
  --interactive-only \
  --require-approval desktop_type_text \
  --require-approval desktop_key \
  --require-approval desktop_click \
  --require-approval desktop_drag
```

SSH provides authentication and encryption; the connector itself listens on
nothing. LunarWing does not pass `SSH_AUTH_SOCK` to stdio servers, and
`BatchMode=yes` cannot prompt, so the key must be usable without an agent or
passphrase (or pass `--env SSH_AUTH_SOCK=...` explicitly). Holding this key
lets the tenant drive the desktop user's session; treat it as such.

## Tools and the operating loop

| Tool | Annotations | Purpose |
|---|---|---|
| `desktop_status` | read-only | Backend, connection, outputs, and the probe report when not connected |
| `desktop_list_outputs` | read-only | Monitor names, logical position and size, scale |
| `desktop_connect` | read-only | Start the session (portal: consent dialog); optional `output` sets the default monitor |
| `desktop_disconnect` | read-only | Release the virtual devices or the portal session; invalidates every frame |
| `desktop_screenshot` | read-only | One image block plus a text block with `frame_id`, output, pixel size, scale and the logical area covered; optional `output`, `region` (zoom) and `format` |
| `desktop_wait` | read-only | Sleep 0.1 to 10 seconds; invalidates every `frame_id` taken before it |
| `desktop_move` | input | Pointer to `(x, y)` |
| `desktop_click` | input | `button` left, middle or right; `count` 1 to 3 |
| `desktop_drag` | input | Press at `from`, move to `to`, release |
| `desktop_scroll` | input | `dx`, `dy` in wheel steps (-20 to 20; positive is down or right), optionally at `(x, y)` |
| `desktop_key` | input | A chord such as `["ctrl", "l"]`, `["Enter"]`, `["ctrl", "shift", "t"]`; `repeat` up to 10 |
| `desktop_type_text` | input | Up to 256 characters; tab and newline allowed, other control characters rejected |
| `desktop_observe` | read-only | Only listed when `--config` is given; see the text fallback below |

Read-only tools declare `{"readOnlyHint": true, "destructiveHint": false}` and
need no approval in LunarWing. Input tools declare `{"readOnlyHint": false,
"destructiveHint": true, "idempotentHint": false, "openWorldHint": true}` and
need approval unless the session auto-approves; `--require-approval` forces
approval for the listed tools regardless, and keeps them out of autonomous
runs.

The loop the tools are built for:

1. `desktop_connect` once. On GNOME or KDE, approve the dialog at the desk
   within about 25 seconds (LunarWing abandons a request after 30 seconds).
2. `desktop_screenshot`. Coordinates for input tools are integer pixels of
   that image, origin top left.
3. One input tool with that screenshot's `frame_id`.
4. `desktop_screenshot` again to see what happened. "sent" only means the
   input was delivered, not that it had the intended effect.
5. `desktop_disconnect` when done.

The frame guard enforces step 2 before every input:

- Only the latest screenshot of each output has a usable `frame_id`.
- A `frame_id` is single use: the first input call that presents it consumes
  it. That call also invalidates the frames of every other output, so each
  input action needs a screenshot taken after the previous input action, even
  with several monitors.
- It expires 60 seconds after the screenshot was returned.
- `desktop_wait` invalidates every frame, because the screen is expected to
  change while waiting.
- Connect, disconnect and portal revocation invalidate all frames. On the
  `wlr` backend an output being added, removed or changing geometry does too,
  including in the middle of an action. On the `portal` backend monitor
  geometry is fixed when sharing starts; see Limitations.
- Coordinates are checked against the frame's pixel size before any input is
  sent; an out-of-range coordinate is rejected without spending the frame.
- A `region` zoom may still be given in the pixels of the last image of an
  output after its frame was used up; only acting on an old image is refused.

LunarWing forwards only the most recent few screenshots of a turn to the model
as images, which is another reason to take a fresh screenshot before acting.

### Coordinates and scaling

Screenshots are downscaled so the long edge is at most `--max-edge` (default
1280) and never sharper than the monitor's physical resolution. The text block
reports the image size and `scale` (image pixels per logical unit). Each pixel
of the image maps to the centre of its area in compositor logical coordinates,
so fractional scaling is handled: a 2560x1600 panel at scale 1.5 is
1707x1067 logical, is captured at 1279 or 1280 by 800 pixels, and a click at
(640, 400) in the image lands at logical (854.17, 534.17). The connector reads
the real image size from the PNG or JPEG header instead of predicting it. On
the `wlr` backend pointer positions are sent in 1/256 logical units (the
precision of Wayland's fixed-point pointer coordinates), and the portal takes
fractional coordinates directly.

`region` takes `x`, `y`, `width`, `height` in the pixels of the latest
screenshot of the same output and returns a sharper image of that area. Its
`frame_id` uses the zoomed image's pixels, and every pixel of a zoomed image
maps to its own pointer position, so a click can target small controls
precisely.

Response size: a screenshot response carries at most 2.5 MiB of base64 (the
daemon drops the connection on a line over 4 MiB). If a PNG is larger, the
connector retries as JPEG (`--jpeg-quality`, then lower quality and smaller
sizes) and says so in the text block; if nothing fits, the call fails and no
frame is issued.

### Keys and text

On the `wlr` backend the connector uploads its own small XKB keymap for each
call, mapping every keysym it needs to a free keycode (as `wtype` does), so
any Unicode text and any named key work regardless of the user's layout.
Keymaps travel through an in-memory file (`memfd`), never through the
filesystem. Modifiers are sent both as key presses and as explicit modifier
state. Every pressed key and modifier is released in reverse order, including
when the call fails or is interrupted.

On the `portal` backend keys are sent as keysyms; characters that the active
layout cannot produce may be dropped by the compositor.

Key names are case-insensitive: modifiers `ctrl`, `shift`, `alt`, `super`
(also `meta`, `win`, `cmd`), `altgr`; named keys `Enter`/`Return`, `Tab`,
`Escape`, `Backspace`, `Delete`, `Insert`, `Space`, arrows, `Home`, `End`,
`PageUp`, `PageDown`, `Menu`, `Print`, `Pause`, `F1`-`F24`, keypad keys and media keys;
or any single character. A capital ASCII letter in a chord is sent as shift
plus the letter. Lock keys are not supported.

## Text fallback for models without vision

With `--config FILE`, `desktop_observe` captures a monitor, sends it to a
LunarVision service for a text description with estimated pixel positions,
and returns that text with a `frame_id`. The file must be a regular file owned
by the connector's user with mode 0600 and contain exactly:

```json
{
  "vision_url": "https://vision.example.net",
  "vision_token": "<LunarVision API token>"
}
```

Only HTTPS, or plain HTTP to a literal loopback address (`127.0.0.1`, `::1`),
is accepted; redirects and environment proxies are disabled; tool arguments
cannot change the endpoint. Both the fresh and the cached response shapes of
the service are understood. Positions from a vision model are estimates;
verify after every action. Without `--config` the tool is not listed.

## Hyprland permissions

Hyprland has an optional permission system (`ecosystem:enforce_permissions`,
off by default). When it is on, screen capture by a non-portal client such as
`grim` defaults to an "ask" prompt, and virtual keyboards are subject to the
`keyboard` permission, matched by device name (`hl-virtual-keyboard`, or
`hl-virtual-keyboard-<process>` with `misc:name_vk_after_proc`). A pending or
denied keyboard has its key events dropped silently. Virtual pointers are not
checked. If screenshots time out or typing has no effect with enforcement on,
add allow rules for `grim` (screencopy) and the virtual keyboard name; see the
Hyprland wiki's permissions page for the syntax of your version.

## Safety

- **Screens contain secrets.** Every screenshot goes to the model provider,
  and every `desktop_observe` description goes to the provider and into
  LunarWing's transcript (screenshot images themselves are kept in memory
  only). LunarWing's secret-leak scanning reads text, not pixels. Close
  password managers, private chats and anything with tokens before
  connecting. Screenshot tools are read-only and need no approval.
- **Screen content is untrusted.** Text on screen, in web pages and in
  documents can carry instructions aimed at the model. The tool descriptions
  and results say so, but that does not stop a model from following them; keep
  approvals on for input tools and watch the session.
- **Approvals.** Register with `--interactive-only` and `--require-approval`
  for the input tools you want confirmed every time. On wlroots compositors
  there is no compositor-side consent dialog, so LunarWing's approval is the
  gate.
- **No listener, no upload.** The connector speaks MCP on stdin/stdout only,
  connects to the local compositor socket and session bus, runs `grim` with an
  argument list (no shell) and reads its image from a pipe. Screenshots and
  typed text are never written to disk by the connector. The portal restore
  token is kept in memory unless `--restore-token-file` is given; that file is
  created with mode 0600 and must stay owner-only.
- One connector controls one desktop session. Do not register the same
  desktop for several tenants.

## Command line

```
computer-use.py [--backend auto|wlr|portal] [--output NAME] [--max-edge PIXELS]
                [--format png|jpeg] [--jpeg-quality Q] [--config FILE]
                [--restore-token-file FILE] [--log-level debug|info|warning|error]
```

`--help` works without PyGObject and without a display. `python3 -m
lunarwing_computer` from this directory is equivalent to the launcher.

`--restore-token-file` (portal only): the connector always asks the portal to
persist the grant (`persist_mode` 1, while the connector runs, with the token in
memory). With a token file it asks for persistence until revoked (`persist_mode`
2) and stores the single-use token in that file, so a later `desktop_connect`
can skip the dialog where the desktop supports it. On GNOME the dialog's
"remember" checkbox decides whether a token is issued. When the desktop side
ends the session (the person stops sharing, or the portal closes it), the
connector forgets the token and deletes the file, so the next
`desktop_connect` shows the dialog again. `desktop_disconnect` keeps the token.

## Limitations

- Not verified on a live compositor (see Status).
- Monitors only: no window capture, no clipboard, no accessibility tree, no
  cursor-position query.
- The locked screen and the login greeter are out of reach.
- `wlr` backend: while the connector types, the compositor hands its custom
  keymap to the focused client and is expected to switch back when the
  physical keyboard is used again; how cleanly each compositor does this has
  not been checked. Input methods (CJK composition) are bypassed. With virtual pointer manager version 1 the
  pointer is mapped across the bounding box of all outputs, which is correct
  only if outputs leave no gaps; version 2 (Hyprland, Sway) binds the pointer
  to the captured output. If the output under the pointer is removed during
  an action, a held button is released on the same virtual pointer and the
  rest of the action is refused with a request for a new screenshot.
- `portal` backend: each capture opens a fresh PipeWire stream; whether a
  static screen delivers a first frame within 5 seconds, and whether stream
  `size` is logical under fractional scaling, has not been checked on GNOME or
  KDE. Keysym typing depends on the active layout.
- `portal` backend: the portal reports monitor geometry once, when sharing
  starts, and the connector cannot refresh it within a session. Every
  screenshot compares the PipeWire stream's pixel size with the size seen on
  the first screenshot of that monitor and closes the session if it changed
  (a resolution or rotation change). A scale change that keeps the pixel size,
  or a change between a screenshot and the next input, is not noticed, and
  input would then be misplaced. After changing monitor settings, call
  `desktop_connect` again.
- Requests are handled one at a time; `notifications/cancelled` does not
  interrupt a running call. JSON-RPC batches are accepted only when protocol
  `2025-03-26` was negotiated (the one MCP revision that has them); under
  `2024-11-05` and `2025-06-18` a batch gets error `-32600`. LunarWing asks
  for `2024-11-05` and never sends batches.
- LunarWing's optional per-server OS sandbox (`--sandbox*` flags) has not been
  tried with the connector; it needs the Wayland socket, the session bus and
  (portal) PipeWire to stay reachable.

## Tests

```bash
cd projects/computer-use
python3 -I -m unittest discover -s tests -v
```

The suite needs no display, D-Bus, network or PyGObject and skips nothing. CI
runs it from `.gitlab/scripts/python_tests.py` in the `python-unit` job.

## Live acceptance checklist

Do these on a desktop you are prepared to have controlled, with nothing
sensitive on screen:

1. `desktop_status` before connecting: the probe shows the expected backend.
2. `desktop_connect`, `desktop_list_outputs`: names and logical geometry match
   `hyprctl monitors -j`, `swaymsg -t get_outputs` or the display settings,
   including fractional scales.
3. `desktop_screenshot` of each output; `region` zoom on small text.
4. Click into a text editor, type a mixed string (ASCII, accented letters,
   Greek, an emoji, a newline), verify it exactly; try `ctrl+a`, Backspace,
   arrows, `ctrl+shift+t` in a browser, scrolling and a drag.
5. Click near each corner of a fractionally scaled output and check the
   pointer position (on Hyprland `hyprctl cursorpos`).
6. Reuse a spent `frame_id`, use a second monitor's `frame_id` after an input
   on the first, use a `frame_id` from before `desktop_wait`, wait 61 seconds,
   and (wlr) hotplug or rescale a monitor: each must be refused without input.
   Unplug a monitor in the middle of a drag on it and confirm the button is
   released. Portal: change a shared monitor's resolution, take a screenshot,
   and confirm the session closes.
7. Portal: deny the dialog, then approve it; revoke sharing from the desktop
   and confirm input stops, then call `desktop_connect` and confirm the dialog
   appears again (with and without `--restore-token-file`); close stdin and
   confirm the sharing indicator disappears.
8. With Hyprland permission enforcement on: confirm the prompts appear and a
   denied keyboard reports no typed text.
