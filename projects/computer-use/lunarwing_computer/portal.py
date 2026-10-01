"""XDG RemoteDesktop + ScreenCast, using the session bus and PipeWire only."""

import os
import secrets
import struct

from .control import DesktopError, Frame

BUS = "org.freedesktop.portal.Desktop"
PATH = "/org/freedesktop/portal/desktop"
REMOTE = "org.freedesktop.portal.RemoteDesktop"
CAST = "org.freedesktop.portal.ScreenCast"
SESSION = "org.freedesktop.portal.Session"
REQUEST = "org.freedesktop.portal.Request"


class Portal:
    def __init__(self):
        try:
            import gi
            gi.require_version("Gio", "2.0")
            gi.require_version("Gst", "1.0")
            gi.require_version("GstApp", "1.0")
            from gi.repository import Gio, GLib, Gst, GstApp
        except (ImportError, ValueError, AttributeError):
            raise DesktopError("Install system PyGObject, GStreamer introspection and PipeWire plugins") from None
        self.Gio, self.GLib, self.Gst = Gio, GLib, Gst
        self.GstApp = GstApp
        Gst.init(None)
        # SSH sessions often omit this even though the same user's desktop bus
        # is already running. Never start a separate dbus-run-session bus.
        if "DBUS_SESSION_BUS_ADDRESS" not in os.environ:
            os.environ["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path=/run/user/{os.getuid()}/bus"
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        self.session = None
        self.subscription = None
        self.stream = None
        self.size = None
        self.serial = None
        self.active = False

    def pump(self):
        context = self.GLib.MainContext.default()
        while context.pending():
            context.iteration(False)

    def call(self, interface, method, signature, values, path=PATH):
        return self.bus.call_sync(
            BUS, path, interface, method, self.GLib.Variant(signature, values),
            None, self.Gio.DBusCallFlags.NONE, 3000, None,
        )

    def request(self, interface, method, signature, values, options):
        token = "lw_" + secrets.token_hex(12)
        sender = self.bus.get_unique_name()[1:].replace(".", "_")
        request_path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
        options = dict(options, handle_token=self.GLib.Variant("s", token))
        loop = self.GLib.MainLoop()
        responses = []

        def response(_bus, _sender, _path, _interface, _signal, params, _data):
            responses.append(params.unpack())
            loop.quit()

        subscription = self.bus.signal_subscribe(
            BUS, REQUEST, "Response", request_path, None,
            self.Gio.DBusSignalFlags.NONE, response, None,
        )
        timed_out = False

        def timeout():
            nonlocal timed_out
            timed_out = True
            loop.quit()
            return False

        timer = self.GLib.timeout_add(20000, timeout)
        completed = False
        try:
            handle = self.call(interface, method, signature, (*values, options)).unpack()[0]
            if handle != request_path:
                raise DesktopError("Portal returned an unexpected request handle")
            if not responses and not timed_out:
                loop.run()
            if not responses:
                raise DesktopError("Desktop portal timed out; retry and approve sharing on the desktop")
            code, result = responses[0]
            if code != 0:
                raise DesktopError("Desktop sharing was cancelled or denied")
            completed = True
            return result
        finally:
            self.bus.signal_unsubscribe(subscription)
            if not timed_out:
                self.GLib.source_remove(timer)
            if not completed:
                try:
                    self.call(REQUEST, "Close", "()", (), path=request_path)
                except Exception:
                    pass

    def start(self):
        self.pump()
        if self.active:
            return
        self.close()
        try:
            result = self.request(REMOTE, "CreateSession", "(a{sv})", (), {
                "session_handle_token": self.GLib.Variant("s", "lw_" + secrets.token_hex(12)),
            })
            self.session = result["session_handle"]
            self.subscription = self.bus.signal_subscribe(
                BUS, SESSION, "Closed", self.session, None,
                self.Gio.DBusSignalFlags.NONE, self.closed, None,
            )
            self.request(REMOTE, "SelectDevices", "(oa{sv})", (self.session,), {
                "types": self.GLib.Variant("u", 3),
            })
            self.request(CAST, "SelectSources", "(oa{sv})", (self.session,), {
                "types": self.GLib.Variant("u", 1),
                "multiple": self.GLib.Variant("b", False),
            })
            result = self.request(REMOTE, "Start", "(osa{sv})", (self.session, ""), {})
            streams = result.get("streams", [])
            if result.get("devices", 0) & 3 != 3 or len(streams) != 1 or not self.session:
                raise DesktopError("Share one monitor and grant both keyboard and pointer access")
            self.stream, properties = streams[0]
            self.size = properties.get("logical_size", properties.get("size"))
            self.serial = properties.get("pipewire-serial")
            if not self.size or len(self.size) != 2 or min(self.size) <= 0 or max(self.size) > 32768:
                raise DesktopError("Portal supplied no usable logical monitor size")
            self.active = True
        except Exception:
            self.close()
            raise

    def closed(self, *_args):
        # Portal revocation must invalidate the PipeWire connection and input.
        self.session = None
        self.close()

    def close(self):
        self.active = False
        session, self.session = self.session, None
        if self.subscription is not None:
            self.bus.signal_unsubscribe(self.subscription)
            self.subscription = None
        self.stream = self.size = self.serial = None
        if session:
            try:
                self.call(SESSION, "Close", "()", (), path=session)
            except Exception:
                pass

    def require_active(self):
        self.pump()
        if not self.active:
            raise DesktopError("No active desktop session; call desktop_connect first")

    def input(self, method, signature, *values):
        self.require_active()
        try:
            self.call(REMOTE, method, signature, (self.session, {}, *values))
        except Exception:
            self.close()
            raise DesktopError("Portal input failed; session closed. Reconnect and observe before retrying") from None

    def move(self, x, y):
        self.input("NotifyPointerMotionAbsolute", "(oa{sv}udd)", self.stream, x, y)

    def button(self, button, pressed):
        self.input("NotifyPointerButton", "(oa{sv}iu)",
                   {"left": 272, "right": 273, "middle": 274}[button], int(pressed))

    def key(self, symbol, pressed):
        self.input("NotifyKeyboardKeysym", "(oa{sv}iu)", symbol, int(pressed))

    def scroll(self, axis, steps):
        self.input("NotifyPointerAxisDiscrete", "(oa{sv}ui)",
                   0 if axis == "vertical" else 1, steps)

    def capture(self):
        self.require_active()
        Gst = self.Gst
        width, height = self.size
        factor = min(1, 1600 / max(width, height))
        target_width, target_height = max(1, round(width * factor)), max(1, round(height * factor))
        # Numeric-only caps and IDs; no external text is interpolated into a pipeline.
        pipeline = Gst.parse_launch(
            "pipewiresrc name=source do-timestamp=true ! videoconvert ! videoscale ! "
            f"video/x-raw,format=RGB,width={target_width},height={target_height} ! "
            "pngenc snapshot=true ! appsink name=sink sync=false max-buffers=1 drop=true"
        )
        # Each one-shot pipeline needs a fresh connection, not a dup of a
        # previously consumed PipeWire protocol socket.
        value, fds = self.bus.call_with_unix_fd_list_sync(
            BUS, PATH, CAST, "OpenPipeWireRemote",
            self.GLib.Variant("(oa{sv})", (self.session, {})),
            self.GLib.VariantType.new("(h)"), self.Gio.DBusCallFlags.NONE,
            3000, None, None,
        )
        fd = fds.get(value.unpack()[0])
        try:
            source = pipeline.get_by_name("source")
            source.set_property("fd", fd)
            if self.serial is not None:
                if source.find_property("target-object") is None:
                    raise DesktopError("Update the GStreamer PipeWire plugin for portal stream serial support")
                source.set_property("target-object", str(self.serial))
            else:
                source.set_property("path", str(self.stream))
            if pipeline.set_state(Gst.State.PLAYING) == Gst.StateChangeReturn.FAILURE:
                raise DesktopError("PipeWire capture could not start")
            sample = pipeline.get_by_name("sink").try_pull_sample(5 * Gst.SECOND)
            if sample is None:
                raise DesktopError("No desktop frame received; check PipeWire and desktop sharing")
            buffer = sample.get_buffer()
            if buffer.get_size() > 8 * 1024 * 1024:
                raise DesktopError("Screenshot exceeds 8 MiB")
            png = buffer.extract_dup(0, buffer.get_size())
            if len(png) < 24 or png[:8] != b"\x89PNG\r\n\x1a\n":
                raise DesktopError("Capture did not produce a PNG")
            actual = struct.unpack(">II", png[16:24])
            self.require_active()
            return Frame(png, *actual, width, height)
        finally:
            pipeline.set_state(Gst.State.NULL)
            os.close(fd)
