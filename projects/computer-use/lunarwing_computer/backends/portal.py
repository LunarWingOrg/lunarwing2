"""XDG RemoteDesktop + ScreenCast portal backend (GNOME, KDE, COSMIC).

Interface facts used here (xdg-desktop-portal 1.22, RemoteDesktop version 2,
ScreenCast version 6):

* CreateSession -> SelectDevices(types, persist_mode, restore_token) ->
  ScreenCast.SelectSources(types, multiple, cursor_mode) on the same session
  -> Start, which normally shows the consent dialog. persist_mode and
  restore_token belong to SelectDevices for a remote-desktop session; a
  restore token is single use and Start returns a new one when persistence
  was granted. "Start succeeded but returned no token" is normal.
* Stream properties: ``size`` and ``position`` are in compositor logical
  coordinates. There is no ``logical_size`` property in ScreenCast.
* Since ScreenCast v6 the stream's ``pipewire-serial`` should be used with
  PipeWire's target-object; the node id is the fallback for older portals.
* NotifyPointerMotionAbsolute takes logical coordinates relative to a stream.

PyGObject is imported only when this backend is used, never at import time.
"""

from __future__ import annotations

import logging
import os
import secrets
import stat
import time
from types import SimpleNamespace
from typing import Any, Callable, Dict, List, Mapping, Optional, Tuple

from .. import control, imageinfo, session_env
from ..control import Chord, DesktopError, Rect
from . import BUTTON_CODES, Backend, BackendOptions, Capture, Output

log = logging.getLogger(__name__)

BUS ="org.freedesktop.portal.Desktop"
PATH = "/org/freedesktop/portal/desktop"
REMOTE = "org.freedesktop.portal.RemoteDesktop"
CAST = "org.freedesktop.portal.ScreenCast"
SESSION = "org.freedesktop.portal.Session"
REQUEST = "org.freedesktop.portal.Request"
PROPERTIES = "org.freedesktop.DBus.Properties"

CONNECT_BUDGET_SECONDS = 25.0  # LunarWing abandons a stdio request after 30 s
CALL_TIMEOUT_MS = 3000
SAMPLE_TIMEOUT_SECONDS = 5
DEVICE_KEYBOARD = 1
DEVICE_POINTER = 2
SOURCE_MONITOR = 1
CURSOR_EMBEDDED = 2
PERSIST_WHILE_RUNNING = 1
PERSIST_UNTIL_REVOKED = 2
MAX_STREAM_EDGE = 32768


def load_gi() -> SimpleNamespace:
    try:
        import gi

        gi.require_version("Gio", "2.0")
        gi.require_version("GLib", "2.0")
        gi.require_version("Gst", "1.0")
        gi.require_version("GstApp", "1.0")
        from gi.repository import Gio, GLib, Gst, GstApp  # noqa: F401  (GstApp enables appsink methods)
    except (ImportError, ValueError, AttributeError):
        raise DesktopError(
            "the portal backend needs the system PyGObject (python3-gobject) with Gio, GStreamer and "
            "GstApp introspection data"
        ) from None
    Gst.init(None)
    return SimpleNamespace(Gio=Gio, GLib=GLib, Gst=Gst)


def open_session_bus(gi: SimpleNamespace, env: Mapping[str, str]):
    address = session_env.discover_dbus_address(env)
    if address is None:
        raise DesktopError(
            "no D-Bus session bus: DBUS_SESSION_BUS_ADDRESS is unset and $XDG_RUNTIME_DIR/bus does not exist"
        )
    flags = gi.Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | gi.Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    try:
        return gi.Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    except Exception:
        raise DesktopError("cannot connect to the D-Bus session bus") from None


def read_property(gi: SimpleNamespace, bus, interface: str, name: str):
    reply = bus.call_sync(
        BUS, PATH, PROPERTIES, "Get", gi.GLib.Variant("(ss)", (interface, name)),
        gi.GLib.VariantType.new("(v)"), gi.Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None,
    )
    return reply.unpack()[0]


def probe_remote_desktop(env: Mapping[str, str], loader: Callable[[], SimpleNamespace] = load_gi,
                         bus_opener: Callable[..., Any] = open_session_bus) -> Dict[str, Any]:
    """Whether the session bus offers org.freedesktop.portal.RemoteDesktop.

    Reads only the interface's ``version`` property; no session is created
    and no dialog is shown.
    """
    try:
        gi = loader()
    except DesktopError as error:
        return {"available": False, "reason": str(error)}
    try:
        bus = bus_opener(gi, env)
    except DesktopError as error:
        return {"available": False, "reason": str(error)}
    try:
        version = read_property(gi, bus, REMOTE, "version")
    except Exception:
        return {"available": False, "reason": f"{REMOTE} is not offered on the session bus"}
    finally:
        try:
            bus.close_sync(None)
        except Exception:
            pass
    return {"available": True, "version": int(version)}


class TokenStore:
    """Restore token file: regular, owned by this user, mode 0600, no symlinks."""

    MAX_BYTES = 4096

    def __init__(self, path: str):
        self.path = path

    def load(self) -> Optional[str]:
        try:
            fd = os.open(self.path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        except FileNotFoundError:
            return None
        except OSError as error:
            raise DesktopError(f"cannot read the restore token file: {error.strerror}") from None
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise DesktopError("the restore token file must be a regular file owned by this user with mode 0600")
            data = os.read(fd, self.MAX_BYTES + 1)
        finally:
            os.close(fd)
        if len(data) > self.MAX_BYTES:
            raise DesktopError("the restore token file is too large")
        token = data.decode("utf-8", "replace").strip()
        return token if _valid_token(token) else None

    def save(self, token: str) -> None:
        if not _valid_token(token):
            return
        directory = os.path.dirname(os.path.abspath(self.path))
        temporary = os.path.join(directory, f".{os.path.basename(self.path)}.{os.getpid()}.{secrets.token_hex(4)}")
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        try:
            os.write(fd, token.encode("ascii") + b"\n")
            os.fsync(fd)
        finally:
            os.close(fd)
        os.replace(temporary, self.path)

    def clear(self) -> None:
        """Delete the token file. A symlink is removed, never followed."""
        try:
            os.unlink(self.path)
        except FileNotFoundError:
            pass


def _valid_token(token: str) -> bool:
    return 0 < len(token) <= 512 and all(33 <= ord(ch) <= 126 for ch in token)


class _Stream:
    def __init__(self, node_id: int, serial: Optional[int], output: Output):
        self.node_id = node_id
        self.serial = serial
        self.output = output
        # PipeWire buffer size seen on the first capture of this session. The
        # portal reports monitor geometry only once, at Start; a later change
        # of the buffer size means the monitor's mode or rotation changed.
        self.pixel_size: Optional[Tuple[int, int]] = None


def negotiated_size(source) -> Optional[Tuple[int, int]]:
    """Width and height of the raw video pipewiresrc negotiated, if known."""
    pad = source.get_static_pad("src")
    caps = pad.get_current_caps() if pad is not None else None
    if caps is None or caps.get_size() < 1:
        return None
    structure = caps.get_structure(0)
    has_width, width = structure.get_int("width")
    has_height, height = structure.get_int("height")
    if not (has_width and has_height) or width <= 0 or height <= 0:
        return None
    return int(width), int(height)


class PortalBackend(Backend):
    name = "portal"

    def __init__(self, env: Optional[Mapping[str, str]] = None, options: Optional[BackendOptions] = None,
                 loader: Callable[[], SimpleNamespace] = load_gi, bus_opener: Callable[..., Any] = open_session_bus,
                 clock: Callable[[], float] = time.monotonic):
        super().__init__()
        self.env = dict(os.environ if env is None else env)
        self.options = options or BackendOptions()
        self._loader = loader
        self._bus_opener = bus_opener
        self._clock = clock
        self.gi: Optional[SimpleNamespace] = None
        self.bus = None
        self.session: Optional[str] = None
        self.subscription: Optional[int] = None
        self.streams: List[_Stream] = []
        self.active = False
        self._restore_token: Optional[str] = None
        self._store = TokenStore(self.options.restore_token_file) if self.options.restore_token_file else None
        # Set when the desktop side ended sharing: the token file is not read
        # again until a new grant replaces it, even if deleting it failed.
        self._stored_token_revoked = False

    # -- D-Bus plumbing ----------------------------------------------------------

    def _ensure_bus(self) -> None:
        if self.gi is None:
            self.gi = self._loader()
        if self.bus is None:
            self.bus = self._bus_opener(self.gi, self.env)

    def pump(self) -> None:
        if self.gi is None:
            return
        context = self.gi.GLib.MainContext.default()
        while context.pending():
            context.iteration(False)

    def _call(self, interface: str, method: str, signature: str, values: Tuple[Any, ...], path: str = PATH):
        assert self.gi is not None and self.bus is not None
        return self.bus.call_sync(
            BUS, path, interface, method, self.gi.GLib.Variant(signature, values),
            None, self.gi.Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None,
        )

    def _property(self, interface: str, name: str, default: int) -> int:
        assert self.gi is not None
        try:
            return int(read_property(self.gi, self.bus, interface, name))
        except Exception:
            return default

    def _request(self, interface: str, method: str, signature: str, values: Tuple[Any, ...],
                 options: Dict[str, Any], deadline: float) -> Dict[str, Any]:
        """Call a portal method that answers through a Request object.

        Subscribes to Request::Response before calling, so an early answer is
        not lost, and waits at most until ``deadline``.
        """
        assert self.gi is not None and self.bus is not None
        GLib, Gio = self.gi.GLib, self.gi.Gio
        token = "lw_" + secrets.token_hex(12)
        sender = self.bus.get_unique_name()[1:].replace(".", "_")
        request_path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
        options = dict(options, handle_token=GLib.Variant("s", token))
        remaining = deadline - self._clock()
        if remaining <= 0:
            raise DesktopError("the desktop portal did not answer in time; call desktop_connect again")
        loop = GLib.MainLoop()
        responses: List[Tuple[int, Dict[str, Any]]] = []

        def on_response(_bus, _sender, _path, _interface, _signal, params, _data):
            responses.append(params.unpack())
            loop.quit()

        subscription = self.bus.signal_subscribe(
            BUS, REQUEST, "Response", request_path, None, Gio.DBusSignalFlags.NONE, on_response, None,
        )
        timed_out = [False]

        def on_timeout():
            timed_out[0] = True
            loop.quit()
            return False

        timer = GLib.timeout_add(max(1, int(remaining * 1000)), on_timeout)
        completed = False
        try:
            handle = self._call(interface, method, signature, (*values, options)).unpack()[0]
            if handle != request_path:
                raise DesktopError("the portal returned an unexpected request handle")
            if not responses and not timed_out[0]:
                loop.run()
            if not responses:
                raise DesktopError(
                    "the desktop did not answer the sharing request in time; approve the dialog on the "
                    "desktop promptly and call desktop_connect again"
                )
            code, results = responses[0]
            if code != 0:
                raise DesktopError("desktop sharing was cancelled or denied on the desktop")
            completed = True
            return results
        finally:
            self.bus.signal_unsubscribe(subscription)
            if not timed_out[0]:
                GLib.source_remove(timer)
            if not completed:
                try:
                    self._call(REQUEST, "Close", "()", (), path=request_path)
                except Exception:
                    pass

    # -- session -----------------------------------------------------------------

    @property
    def connected(self) -> bool:
        self.pump()
        return self.active

    def connect(self) -> None:
        self._ensure_bus()
        self.pump()
        if self.active:
            return
        self._close_session()
        assert self.gi is not None
        GLib = self.gi.GLib
        deadline = self._clock() + CONNECT_BUDGET_SECONDS
        try:
            rd_version = self._property(REMOTE, "version", 1)
            cursor_modes = self._property(CAST, "AvailableCursorModes", 0)
            result = self._request(REMOTE, "CreateSession", "(a{sv})", (), {
                "session_handle_token": GLib.Variant("s", "lw_" + secrets.token_hex(12)),
            }, deadline)
            self.session = str(result["session_handle"])
            self.subscription = self.bus.signal_subscribe(
                BUS, SESSION, "Closed", self.session, None, self.gi.Gio.DBusSignalFlags.NONE, self._on_closed, None,
            )
            devices: Dict[str, Any] = {"types": GLib.Variant("u", DEVICE_KEYBOARD | DEVICE_POINTER)}
            if rd_version >= 2:
                devices["persist_mode"] = GLib.Variant(
                    "u", PERSIST_UNTIL_REVOKED if self._store else PERSIST_WHILE_RUNNING)
                token = self._restore_token or self._load_stored_token()
                if token:
                    devices["restore_token"] = GLib.Variant("s", token)
            self._request(REMOTE, "SelectDevices", "(oa{sv})", (self.session,), devices, deadline)
            sources: Dict[str, Any] = {
                "types": GLib.Variant("u", SOURCE_MONITOR),
                "multiple": GLib.Variant("b", True),
            }
            if cursor_modes & CURSOR_EMBEDDED:
                sources["cursor_mode"] = GLib.Variant("u", CURSOR_EMBEDDED)
            self._request(CAST, "SelectSources", "(oa{sv})", (self.session,), sources, deadline)
            result = self._request(REMOTE, "Start", "(osa{sv})", (self.session, ""), {}, deadline)
            if int(result.get("devices", 0)) & (DEVICE_KEYBOARD | DEVICE_POINTER) != DEVICE_KEYBOARD | DEVICE_POINTER:
                raise DesktopError("grant both keyboard and pointer access in the sharing dialog")
            self.streams = self._parse_streams(result.get("streams", []))
            token = result.get("restore_token")
            # A token is single use: forget the old one even if no new one came back.
            self._restore_token = token if isinstance(token, str) and _valid_token(token) else None
            if self._store is not None and self._restore_token:
                self._store.save(self._restore_token)
                self._stored_token_revoked = False
            self.active = True
            self.generation += 1
        except BaseException:
            self._close_session()
            raise

    @staticmethod
    def _parse_streams(streams) -> List[_Stream]:
        if not streams:
            raise DesktopError("select at least one monitor in the sharing dialog")
        parsed = []
        for index, (node_id, properties) in enumerate(streams):
            size = properties.get("size")
            if not size or len(size) != 2 or min(size) <= 0 or max(size) > MAX_STREAM_EDGE:
                raise DesktopError("the portal supplied no usable stream size")
            position = properties.get("position") or (0, 0)
            serial = properties.get("pipewire-serial")
            output = Output(f"monitor-{index}", int(position[0]), int(position[1]), int(size[0]), int(size[1]))
            parsed.append(_Stream(int(node_id), int(serial) if serial is not None else None, output))
        return parsed

    def _on_closed(self, *_args) -> None:
        # The desktop side ended the session: the person stopped sharing, or
        # the portal closed it. Our own close unsubscribes first, so this never
        # runs for desktop_disconnect. Drop all input, and forget the restore
        # token so the next desktop_connect asks the person again instead of
        # quietly resuming a share they ended.
        self.session = None
        self._forget_restore_token()
        self._close_session()

    def _load_stored_token(self) -> Optional[str]:
        if self._store is None or self._stored_token_revoked:
            return None
        return self._store.load()

    def _forget_restore_token(self) -> None:
        self._restore_token = None
        if self._store is None:
            return
        self._stored_token_revoked = True
        try:
            self._store.clear()
        except OSError as error:
            # Called from a D-Bus signal handler: never raise. The flag above
            # still keeps this process from reusing the file.
            log.warning("could not delete the restore token file: %s", error.strerror)

    def _close_session(self) -> None:
        was_active = self.active
        self.active = False
        session, self.session = self.session, None
        if self.subscription is not None and self.bus is not None:
            try:
                self.bus.signal_unsubscribe(self.subscription)
            except Exception:
                pass
        self.subscription = None
        self.streams = []
        if was_active:
            self.generation += 1
        if session:
            try:
                self._call(SESSION, "Close", "()", (), path=session)
            except Exception:
                pass

    def disconnect(self) -> None:
        self._close_session()

    def describe(self) -> Dict[str, Any]:
        info: Dict[str, Any] = {"backend": self.name, "connected": self.connected, "streams": len(self.streams)}
        if self._store is not None and self._stored_token_revoked:
            info["restore_token"] = "forgotten because the desktop ended sharing"
        elif self._store is not None:
            info["restore_token"] = "stored in the configured file"
        else:
            info["restore_token"] = "kept in memory only" if self._restore_token else "none"
        return info

    def outputs(self) -> List[Output]:
        self._require_active()
        return [stream.output for stream in self.streams]

    def _require_active(self) -> None:
        self.pump()
        if not self.active:
            raise DesktopError("the desktop session is not connected; call desktop_connect")

    def _stream_for(self, output: Output) -> _Stream:
        for stream in self.streams:
            if stream.output.name == output.name:
                return stream
        raise DesktopError(f"unknown output {output.name}")

    # -- capture -------------------------------------------------------------------

    def capture(self, output: Output, max_edge: int, fmt: str, region: Optional[Rect] = None,
                jpeg_quality: int = 80) -> Capture:
        self._require_active()
        assert self.gi is not None
        Gst, GLib, Gio = self.gi.Gst, self.gi.GLib, self.gi.Gio
        stream = self._stream_for(output)
        rect = region or control.full_rect(output.width, output.height)
        scale = control.capture_scale(rect, max_edge, self.max_capture_scale(output))
        full_width = max(1, round(output.width * scale))
        full_height = max(1, round(output.height * scale))
        # Only integers are interpolated into the pipeline description.
        description = (
            "pipewiresrc name=source do-timestamp=true ! videoconvert ! videoscale ! "
            f"video/x-raw,format=RGB,width={full_width},height={full_height}"
        )
        if region is not None:
            left = min(max(0, round(rect.x * scale)), full_width - 1)
            top = min(max(0, round(rect.y * scale)), full_height - 1)
            right = min(max(0, full_width - round((rect.x + rect.width) * scale)), full_width - left - 1)
            bottom = min(max(0, full_height - round((rect.y + rect.height) * scale)), full_height - top - 1)
            description += f" ! videocrop left={left} right={right} top={top} bottom={bottom}"
        if fmt == "jpeg":
            description += f" ! jpegenc quality={int(jpeg_quality)}"
        else:
            description += " ! pngenc snapshot=true"
        description += " ! appsink name=sink sync=false max-buffers=1 drop=true"
        pipeline = Gst.parse_launch(description)
        # A fresh PipeWire remote per capture; a consumed remote cannot be reused.
        value, fds = self.bus.call_with_unix_fd_list_sync(
            BUS, PATH, CAST, "OpenPipeWireRemote", GLib.Variant("(oa{sv})", (self.session, {})),
            GLib.VariantType.new("(h)"), Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None, None,
        )
        fd = fds.get(value.unpack()[0])
        try:
            source = pipeline.get_by_name("source")
            source.set_property("fd", fd)
            if stream.serial is not None:
                if source.find_property("target-object") is None:
                    raise DesktopError("update the GStreamer PipeWire plugin; it lacks target-object support")
                source.set_property("target-object", str(stream.serial))
            else:
                source.set_property("path", str(stream.node_id))
            # Damage-driven streams may not send a new buffer for a static
            # screen; ask pipewiresrc to resend the last one.
            if source.find_property("keepalive-time") is not None:
                source.set_property("keepalive-time", 1000)
            if source.find_property("resend-last") is not None:
                source.set_property("resend-last", True)
            if pipeline.set_state(Gst.State.PLAYING) == Gst.StateChangeReturn.FAILURE:
                raise DesktopError("PipeWire capture could not start")
            sample = pipeline.get_by_name("sink").try_pull_sample(SAMPLE_TIMEOUT_SECONDS * Gst.SECOND)
            if sample is None:
                raise DesktopError("no desktop frame arrived from PipeWire; check desktop sharing and retry")
            self._check_stream_size(stream, negotiated_size(source))
            buffer = sample.get_buffer()
            data = buffer.extract_dup(0, buffer.get_size())
            try:
                mime, width, height = imageinfo.image_size(data)
            except ValueError:
                raise DesktopError("the capture pipeline produced no readable image") from None
            self._require_active()
            return Capture(data, mime, width, height)
        finally:
            pipeline.set_state(Gst.State.NULL)
            os.close(fd)

    def _check_stream_size(self, stream: _Stream, size: Optional[Tuple[int, int]]) -> None:
        """Close the session when a monitor's pixel size changed since the first capture.

        The logical geometry used for coordinate mapping was fixed at Start
        and cannot be refreshed without a new session, so frames and input
        based on it would be misplaced.
        """
        if size is None:
            return
        if stream.pixel_size is None:
            stream.pixel_size = size
            return
        if size != stream.pixel_size:
            before = stream.pixel_size
            self._close_session()
            raise DesktopError(
                f"the shared monitor {stream.output.name} changed from {before[0]}x{before[1]} to "
                f"{size[0]}x{size[1]} pixels; the session was closed, call desktop_connect again"
            )

    # -- input ---------------------------------------------------------------------

    def _input(self, method: str, signature: str, *values: Any) -> None:
        self._require_active()
        try:
            self._call(REMOTE, method, signature, (self.session, {}, *values))
        except Exception:
            self._close_session()
            raise DesktopError("portal input failed and the session was closed; call desktop_connect again") from None

    def pointer_move_abs(self, output: Output, x: float, y: float) -> None:
        stream = self._stream_for(output)
        x = min(max(float(x), 0.0), output.width - 1.0)
        y = min(max(float(y), 0.0), output.height - 1.0)
        self._input("NotifyPointerMotionAbsolute", "(oa{sv}udd)", stream.node_id, x, y)

    def button(self, button: str, pressed: bool) -> None:
        if button not in BUTTON_CODES:
            raise DesktopError("button must be left, middle or right")
        self._input("NotifyPointerButton", "(oa{sv}iu)", BUTTON_CODES[button], 1 if pressed else 0)

    def scroll(self, dx: int, dy: int) -> None:
        if dy:
            self._input("NotifyPointerAxisDiscrete", "(oa{sv}ui)", 0, int(dy))  # 0 = vertical
        if dx:
            self._input("NotifyPointerAxisDiscrete", "(oa{sv}ui)", 1, int(dx))  # 1 = horizontal

    def _keysym(self, keysym: int, pressed: bool) -> None:
        self._input("NotifyKeyboardKeysym", "(oa{sv}iu)", keysym, 1 if pressed else 0)

    def key_chord(self, chord: Chord, repeat: int = 1) -> None:
        symbols = [m.keysym for m in chord.modifiers] + ([chord.key] if chord.key is not None else [])
        for _ in range(repeat):
            held: List[int] = []
            try:
                for symbol in symbols:
                    self._keysym(symbol, True)
                    held.append(symbol)
            finally:
                failed = False
                for symbol in reversed(held):
                    try:
                        self._keysym(symbol, False)
                    except DesktopError:
                        failed = True
                if failed:
                    raise DesktopError("key input was interrupted; the session was closed to release held keys")

    def type_text(self, text: str) -> None:
        for symbol in control.text_keysyms(text):
            self._keysym(symbol, True)
            self._keysym(symbol, False)
