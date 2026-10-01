# LunarWing computer use — GNU/Linux Wayland

An MCP connector for controlling an **existing desktop session**. Run it as
the desktop user, locally or over SSH. It uses the XDG RemoteDesktop and
ScreenCast portals for input and PipeWire for screenshots. No root, X11
fallback, virtual desktop, input-device access, or network listener is needed.

**Status: initial implementation, awaiting live compositor acceptance.**
The automated suite exercises the controller, MCP protocol, portal lifecycle
and LunarVision boundary with test doubles. It does not establish that a
particular GNOME/KDE release works; run the acceptance steps below on the
actual desktop before relying on it.

## Requirements on the desktop machine

- Python 3.10+ and the distribution's **PyGObject** bindings (`gi`). Use the
  system Python; a virtualenv must expose system site packages.
- GObject introspection data for Gio, GLib, GStreamer and GstApp.
- GStreamer plugins providing `pipewiresrc`, `videoconvert`, `videoscale`,
  `pngenc`, and `appsink`.
- PipeWire, `xdg-desktop-portal`, and a desktop backend implementing **both
  RemoteDesktop and ScreenCast**. GNOME and KDE have such implementations;
  a screenshot-only or ScreenCast-only backend is insufficient. Wayland
  alone does not guarantee remote-input support.
- A logged-in graphical session, with its user D-Bus session bus reachable.
- For LunarWing's visual observations: an operator-configured LunarVision
  service with its **VL backend enabled**, reachable from the desktop machine.

The input backend uses the portal's D-Bus `Notify*` methods. It does not open
an EIS connection or mix input protocols. A future libei backend can replace
these methods without changing the MCP tools.

Check the Python/plugin installation without opening a desktop session:

```bash
python3 -c 'import gi; gi.require_version("Gst", "1.0"); gi.require_version("GstApp", "1.0"); from gi.repository import Gio, Gst, GstApp; Gst.init(None); print({n: Gst.ElementFactory.find(n) is not None for n in ("pipewiresrc", "videoconvert", "videoscale", "pngenc", "appsink")})'
```

The connector uses the existing `DBUS_SESSION_BUS_ADDRESS`, or the same UID's
`/run/user/<uid>/bus` when that variable is missing (common over SSH). Do not
wrap it in `dbus-run-session`: that creates a different bus. On unusual
session-bus layouts, provide the correct address in the launcher. This works
independently of systemd or OpenRC; it is a session process, not an mt-admin
tenant service. Run only one connector against a desktop at a time.

## Configure visual observations

LunarWing currently converts MCP results to text and does not pass MCP image
blocks to its model. `desktop_observe` therefore captures the selected monitor
and asks LunarVision to describe it and locate controls. The model receives
the description, screenshot dimensions and a `frame_id`. OCR alone is not
enough for this operation. VL coordinates are estimates; verify every action
with another observation.

Create `~/.config/lunarwing-computer.json`, owned by the desktop user with mode
`0600`, using the actual LunarVision API origin and token:

```json
{
  "vision_url": "https://vision.example.net",
  "vision_token": "REPLACE_WITH_YOUR_TOKEN"
}
```

Screenshots go to this endpoint's `/v1/vision/analyze` route. Only HTTPS or
literal loopback HTTP is accepted. Redirects and ambient HTTP proxies are
disabled; tool arguments cannot change the endpoint. Use the actual allocated
API port for your tenant, not its health port. No screenshot or typed text is
written to disk by the connector; observations returned to LunarWing may be
retained in its tool transcript.

`desktop_screenshot` returns a standard MCP image for image-capable clients.
It works without LunarVision, but **does not give LunarWing visual perception**.
Use `desktop_observe` with LunarWing.

## Local connection

Keep a checkout on the desktop and test the launcher's help:

```bash
python3 /absolute/path/lunarwing2/projects/computer-use/computer-use.py --help
```

If LunarWing runs as the same desktop user:

```bash
lunarwing mcp add desktop --transport stdio \
  --command /usr/bin/python3 \
  --arg /absolute/path/lunarwing2/projects/computer-use/computer-use.py \
  --arg=--config --arg /home/desktop-user/.config/lunarwing-computer.json
```

Activate the installed extension through LunarWing's existing extension UI
or `tool_activate`. Installation alone does not activate it. Input tools
declare side effects and use the existing MCP approval policy.

## Remote desktop over SSH

On the desktop machine create a private launcher, for example
`/home/alice/.local/bin/lunarwing-computer`, with these contents:

```sh
#!/bin/sh
exec /usr/bin/python3 /home/alice/lunarwing2/projects/computer-use/computer-use.py \
  --config /home/alice/.config/lunarwing-computer.json
```

Make it executable. On the LunarWing host configure an SSH alias `my-desktop`
with user `alice`, an identity file, and a verified known-host entry. Then:

```bash
lunarwing mcp add desktop --transport stdio \
  --command /usr/bin/ssh \
  --arg=-T --arg=-o --arg=BatchMode=yes \
  --arg=-o --arg=StrictHostKeyChecking=yes \
  --arg my-desktop --arg /home/alice/.local/bin/lunarwing-computer
```

The SSH account is the desktop account. The connector's config/token stay on
that machine. No shell profile or launcher may print to stdout: it carries
newline-delimited MCP JSON only. SSH handles authentication and encryption.
Use a dedicated SSH key and, where suitable, an SSH forced-command launcher
to constrain that key to the connector.

## Tools and operating loop

| Tool | Purpose |
|------|---------|
| `desktop_status` | Session and vision configuration status |
| `desktop_connect` | Request sharing of one existing monitor plus pointer/keyboard |
| `desktop_observe` | Screenshot → LunarVision description, dimensions, `frame_id` |
| `desktop_screenshot` | PNG for image-capable MCP clients |
| `desktop_move` | Pointer movement/hover |
| `desktop_click` | Left/middle/right, single/double click |
| `desktop_scroll` | Vertical/horizontal discrete wheel input |
| `desktop_key` | Named keys and chords such as `["Ctrl", "l"]` |
| `desktop_type_text` | Up to 256 characters, using Unicode keysyms |
| `desktop_disconnect` | Release the portal session |

1. Call `desktop_connect`; select a monitor and accept keyboard/pointer sharing
   **on the physical desktop**, within the portal request's 20-second timeout.
2. Call `desktop_observe` with a concrete question such as “Locate the address
   bar and describe the current browser page.”
3. Use that result's `frame_id` for one input call. Coordinates refer to the
   returned image's pixels (origin top-left), not physical monitor pixels.
   The connector maps them to portal logical coordinates for fractional scaling.
4. Observe again to confirm the effect. `sent: true` only confirms input was
   submitted; it is not proof of application/task success.
5. Disconnect when finished. The desktop user can revoke sharing using the
   compositor's sharing controls at any time.

Every input consumes the frame ID; IDs also expire after 60 seconds or on
disconnect/reconnect. Invalid coordinates/arguments are rejected before input.
Key chords release modifiers in reverse order, including on failure. Portal
input errors close the session. There are no raw key-down tools that leave
keys pressed between calls. Text and screen content are untrusted application
data, not new instructions to the agent.

MCP tool calls are processed sequentially. Notifications never execute input.
Disconnecting stdio/SSH or terminating the process closes its session. There
is no persistent permission/restore token; reconnecting requires portal
selection again. During a synchronous call, EOF is handled when that call
returns; portal requests, screenshot acquisition and vision HTTP I/O have
timeouts. `notifications/cancelled` is not an input rollback mechanism.

Initial scope: one monitor, screenshots, hover/click, scroll, chords and text.
Clipboard, accessibility-tree inspection, drag-and-drop, session persistence,
multiple monitors and unattended login/unlock are not implemented. Unicode
keysyms depend on compositor support; verify text in the target app. Reconnect
after changing display layout/scale or hotplugging a monitor.

## Verification

Run the tests without GI or a graphical session:

```bash
cd projects/computer-use
python3 -m unittest discover -s tests -v
```

Live acceptance (requires the designated Wayland desktop):

1. Connect with MCP and approve sharing; cancel a second connect attempt after
   disconnecting and confirm it returns a tool error, not success.
2. Observe a blank text editor. Confirm image dimensions, description and
   coordinates, including under fractional scaling.
3. Click the editor, observe, type a short test string, observe; verify the
   exact text. Exercise Ctrl+A, Backspace, arrow keys, scrolling and hover.
4. Attempt an action with the consumed frame ID; verify no second input occurs.
5. Revoke sharing from the desktop; verify screenshots and input fail until
   a new portal session is explicitly approved.
6. Close SSH/stdin and verify the desktop sharing indicator disappears.
7. Test a failed/redirecting LunarVision endpoint; verify it returns an error
   and never makes a previous screenshot actionable.

Portal API references:
[RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html),
[ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html).
