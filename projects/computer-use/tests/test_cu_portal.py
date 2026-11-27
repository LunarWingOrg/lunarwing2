"""Portal backend with fake D-Bus, GLib and GStreamer objects (no session bus, no dialog)."""

import os
import stat
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

import cu_support  # noqa: F401
from cu_support import make_png

from lunarwing_computer.backends import BackendOptions, Output
from lunarwing_computer.backends import portal as portal_module
from lunarwing_computer.backends.portal import CAST, PROPERTIES, REMOTE, REQUEST, SESSION, PortalBackend, TokenStore
from lunarwing_computer.control import DesktopError, Rect, parse_chord


class Variant:
    def __init__(self, signature, value):
        self.signature = signature
        self.value = value

    def unpack(self):
        return self.value

    def __repr__(self):
        return f"Variant({self.signature!r}, {self.value!r})"


def plain(options):
    return {key: value.value for key, value in options.items()}


class FakeLoop:
    def __init__(self, fake):
        self.fake = fake

    def run(self):
        if self.fake.pending:
            callback, params = self.fake.pending.pop(0)
            callback(None, None, None, None, None, params, None)
        elif self.fake.timers:
            _id, callback = self.fake.timers.popitem()
            callback()

    def quit(self):
        self.fake.quits += 1


class FakeFdList:
    def __init__(self, fd):
        self.fd = fd

    def get(self, _index):
        return self.fd


class FakeStructure:
    def __init__(self, fields):
        self.fields = fields

    def get_int(self, name):
        # PyGObject returns (found, value) for Gst.Structure.get_int.
        return (name in self.fields, self.fields.get(name, 0))


class FakeCaps:
    def __init__(self, fields):
        self.structure = FakeStructure(fields)

    def get_size(self):
        return 1

    def get_structure(self, _index):
        return self.structure


class FakeSource:
    def __init__(self, fake, props=("fd", "path", "target-object", "keepalive-time", "resend-last")):
        self.fake = fake
        self.props = set(props)
        self.values = {}

    def find_property(self, name):
        return object() if name in self.props else None

    def set_property(self, name, value):
        self.values[name] = value

    def get_static_pad(self, name):
        assert name == "src"
        size = self.fake.stream_pixel_size
        caps = None if size is None else FakeCaps({"width": size[0], "height": size[1]})
        return SimpleNamespace(get_current_caps=lambda: caps)


class FakeBuffer:
    def __init__(self, data):
        self.data = data

    def get_size(self):
        return len(self.data)

    def extract_dup(self, offset, size):
        return self.data[offset:offset + size]


class FakePipeline:
    def __init__(self, description, fake):
        self.description = description
        self.fake = fake
        self.source = FakeSource(fake)
        self.states = []

    def get_by_name(self, name):
        if name == "source":
            return self.source
        fake = self.fake

        class Sink:
            def try_pull_sample(self, timeout):
                if fake.sample is None:
                    return None
                return SimpleNamespace(get_buffer=lambda: FakeBuffer(fake.sample))

        return Sink()

    def set_state(self, state):
        self.states.append(state)
        return 1


class FakePortal:
    """Fake session bus + portal frontend, recording calls."""

    def __init__(self, properties=None, start_result=None, early=False, respond=True):
        self.properties = properties if properties is not None else {
            (REMOTE, "version"): 2, (REMOTE, "AvailableDeviceTypes"): 7, (CAST, "AvailableCursorModes"): 3,
        }
        self.start_result = start_result if start_result is not None else {
            "devices": 3, "restore_token": "token-1",
            "streams": [(41, {"size": (2560, 1440), "position": (0, 0), "pipewire-serial": 99}),
                        (42, {"size": (1280, 720), "position": (2560, 0)})],
        }
        self.early = early
        self.respond = respond
        self.calls = []
        self.subscriptions = {}
        self.unsubscribed = []
        self.pending = []
        self.timers = {}
        self.removed_timers = []
        self.quits = 0
        self.fds = []
        self.pipelines = []
        self.sample = make_png(1280, 720)
        self.stream_pixel_size = (2560, 1440)  # what pipewiresrc negotiates; None means no caps
        self.fail_notify = False
        self.next_id = 1

        fake = self

        class MainContext:
            @staticmethod
            def default():
                return SimpleNamespace(pending=lambda: False, iteration=lambda _block: None)

        def timeout_add(ms, callback):
            fake.next_id += 1
            fake.timers[fake.next_id] = callback
            return fake.next_id

        def source_remove(timer_id):
            fake.removed_timers.append(timer_id)
            fake.timers.pop(timer_id, None)

        self.gi = SimpleNamespace(
            GLib=SimpleNamespace(Variant=Variant, MainLoop=lambda: FakeLoop(fake), timeout_add=timeout_add,
                                 source_remove=source_remove, MainContext=MainContext,
                                 VariantType=SimpleNamespace(new=lambda signature: signature)),
            Gio=SimpleNamespace(DBusSignalFlags=SimpleNamespace(NONE=0), DBusCallFlags=SimpleNamespace(NONE=0),
                                DBusConnectionFlags=SimpleNamespace(AUTHENTICATION_CLIENT=1, MESSAGE_BUS_CONNECTION=2)),
            Gst=SimpleNamespace(parse_launch=lambda description: fake._pipeline(description), SECOND=10 ** 9,
                                State=SimpleNamespace(PLAYING=4, NULL=1),
                                StateChangeReturn=SimpleNamespace(FAILURE=0)),
        )

    def _pipeline(self, description):
        pipeline = FakePipeline(description, self)
        self.pipelines.append(pipeline)
        return pipeline

    # Gio.DBusConnection surface
    def get_unique_name(self):
        return ":1.23"

    def signal_subscribe(self, bus, interface, signal, path, arg0, flags, callback, data):
        self.next_id += 1
        self.subscriptions[self.next_id] = (interface, signal, path, callback)
        return self.next_id

    def signal_unsubscribe(self, subscription):
        self.unsubscribed.append(subscription)
        self.subscriptions.pop(subscription, None)

    def close_sync(self, _cancellable):
        pass

    def methods(self):
        return [(c[0], c[1]) for c in self.calls]

    def call_sync(self, bus, path, interface, method, parameters, reply_type, flags, timeout, cancellable):
        values = parameters.value
        self.calls.append((interface, method, path, values))
        if interface == PROPERTIES and method == "Get":
            key = tuple(values)
            if key not in self.properties:
                raise RuntimeError("org.freedesktop.DBus.Error.UnknownInterface")
            return Variant("(v)", (self.properties[key],))
        if method.startswith("Notify"):
            if self.fail_notify:
                raise RuntimeError("private D-Bus diagnostic")
            return Variant("()", ())
        if method == "Close":
            return Variant("()", ())
        options = values[-1]
        token = options["handle_token"].value
        request_path = f"/org/freedesktop/portal/desktop/request/1_23/{token}"
        callback = next(cb for (i, s, p, cb) in self.subscriptions.values() if p == request_path)
        if method == "CreateSession":
            results = {"session_handle": "/org/freedesktop/portal/desktop/session/1_23/s1"}
        elif method == "Start":
            results = self.start_result
        else:
            results = {}
        if self.respond:
            params = Variant("(ua{sv})", (0, results))
            if self.early:
                callback(None, None, None, None, None, params, None)
            else:
                self.pending.append((callback, params))
        return Variant("(o)", (request_path,))

    def call_with_unix_fd_list_sync(self, bus, path, interface, method, parameters, reply_type, flags, timeout,
                                    fd_list, cancellable):
        self.calls.append((interface, method, path, parameters.value))
        fd = os.open(os.devnull, os.O_RDONLY)
        self.fds.append(fd)
        return Variant("(h)", (0,)), FakeFdList(fd)


def backend_for(fake, **options):
    return PortalBackend(env={}, options=BackendOptions(**options), loader=lambda: fake.gi,
                         bus_opener=lambda gi, env: fake)


class RequestTests(unittest.TestCase):
    def test_subscribes_before_call_and_accepts_an_early_response(self):
        fake = FakePortal(early=True)
        backend = backend_for(fake)
        backend._ensure_bus()
        result = backend._request(REMOTE, "CreateSession", "(a{sv})", (), {}, deadline=backend._clock() + 5)
        self.assertIn("session_handle", result)
        self.assertEqual(fake.quits, 1)
        self.assertEqual(len(fake.unsubscribed), 1)
        self.assertEqual(len(fake.removed_timers), 1)
        self.assertNotIn((REQUEST, "Close"), fake.methods())

    def test_timeout_closes_the_pending_request(self):
        fake = FakePortal(respond=False)
        backend = backend_for(fake)
        backend._ensure_bus()
        with self.assertRaisesRegex(DesktopError, "in time"):
            backend._request(REMOTE, "Start", "(osa{sv})", ("/s", ""), {}, deadline=backend._clock() + 5)
        close = [c for c in fake.calls if c[:2] == (REQUEST, "Close")]
        self.assertEqual(len(close), 1)
        self.assertTrue(close[0][2].startswith("/org/freedesktop/portal/desktop/request/1_23/lw_"))
        self.assertEqual(len(fake.unsubscribed), 1)
        self.assertEqual(fake.removed_timers, [])

    def test_denied_dialog_is_an_error(self):
        fake = FakePortal()
        backend = backend_for(fake)
        backend._ensure_bus()
        original = fake.call_sync

        def deny(*args):
            reply = original(*args)
            if fake.pending:
                callback, _params = fake.pending.pop()
                fake.pending.append((callback, Variant("(ua{sv})", (1, {}))))
            return reply

        fake.call_sync = deny
        with self.assertRaisesRegex(DesktopError, "cancelled or denied"):
            backend._request(REMOTE, "Start", "(osa{sv})", ("/s", ""), {}, deadline=backend._clock() + 5)


class SessionTests(unittest.TestCase):
    def test_connect_sequence_options_and_streams(self):
        fake = FakePortal()
        backend = backend_for(fake)
        backend.connect()
        portal_calls = [c for c in fake.calls if c[0] in (REMOTE, CAST)]
        self.assertEqual([c[1] for c in portal_calls], ["CreateSession", "SelectDevices", "SelectSources", "Start"])
        devices = plain(portal_calls[1][3][-1])
        self.assertEqual(devices["types"], 3)
        self.assertEqual(devices["persist_mode"], 1)
        self.assertNotIn("restore_token", devices)
        sources = plain(portal_calls[2][3][-1])
        self.assertEqual((sources["types"], sources["multiple"], sources["cursor_mode"]), (1, True, 2))
        self.assertTrue(backend.connected)
        self.assertEqual(backend.outputs(), [Output("monitor-0", 0, 0, 2560, 1440), Output("monitor-1", 2560, 0, 1280, 720)])
        self.assertEqual(backend.describe()["restore_token"], "kept in memory only")
        backend.disconnect()
        self.assertIn((SESSION, "Close"), fake.methods())
        fake.calls.clear()
        backend.connect()
        devices = plain([c for c in fake.calls if c[1] == "SelectDevices"][0][3][-1])
        self.assertEqual(devices["restore_token"], "token-1")

    def test_missing_grants_close_the_session(self):
        fake = FakePortal(start_result={"devices": 2, "streams": [(41, {"size": (800, 600)})]})
        backend = backend_for(fake)
        with self.assertRaisesRegex(DesktopError, "keyboard and pointer"):
            backend.connect()
        self.assertFalse(backend.connected)
        self.assertIn((SESSION, "Close"), fake.methods())
        fake = FakePortal(start_result={"devices": 3, "streams": []})
        with self.assertRaisesRegex(DesktopError, "at least one monitor"):
            backend_for(fake).connect()

    def test_old_portal_without_version_2_gets_no_persistence_or_cursor(self):
        fake = FakePortal(properties={})
        backend = backend_for(fake)
        backend.connect()
        devices = plain([c for c in fake.calls if c[1] == "SelectDevices"][0][3][-1])
        sources = plain([c for c in fake.calls if c[1] == "SelectSources"][0][3][-1])
        self.assertEqual(set(devices), {"types", "handle_token"})
        self.assertNotIn("cursor_mode", sources)

    def test_revocation_invalidates_input(self):
        fake = FakePortal()
        backend = backend_for(fake)
        backend.connect()
        generation = backend.generation
        closed = [cb for (i, s, p, cb) in fake.subscriptions.values() if s == "Closed"][0]
        closed()
        self.assertFalse(backend.connected)
        self.assertGreater(backend.generation, generation)
        fake.calls.clear()
        with self.assertRaisesRegex(DesktopError, "desktop_connect"):
            backend.key_chord(parse_chord(["a"]))
        self.assertEqual([c for c in fake.calls if c[1].startswith("Notify")], [])

    def test_desktop_side_close_forgets_the_restore_token(self):
        # Review ANN-1: after the person stops sharing, reconnecting must ask
        # them again instead of resuming the share with the old token.
        fake = FakePortal()
        backend = backend_for(fake)
        backend.connect()
        closed = [cb for (i, s, p, cb) in fake.subscriptions.values() if s == "Closed"][0]
        closed()
        self.assertEqual(backend.describe()["restore_token"], "none")
        fake.calls.clear()
        backend.connect()
        devices = plain([c for c in fake.calls if c[1] == "SelectDevices"][0][3][-1])
        self.assertEqual(devices["persist_mode"], 1)
        self.assertNotIn("restore_token", devices)


class RestoreTokenFileTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cu-token-")
        self.path = os.path.join(self.tmp.name, "restore-token")

    def tearDown(self):
        self.tmp.cleanup()

    def test_token_is_saved_0600_and_reused_with_persist_mode_2(self):
        fake = FakePortal()
        backend_for(fake, restore_token_file=self.path).connect()
        info = os.stat(self.path)
        self.assertEqual(stat.S_IMODE(info.st_mode), 0o600)
        with open(self.path) as handle:
            self.assertEqual(handle.read().strip(), "token-1")
        fake = FakePortal()
        backend_for(fake, restore_token_file=self.path).connect()
        devices = plain([c for c in fake.calls if c[1] == "SelectDevices"][0][3][-1])
        self.assertEqual((devices["persist_mode"], devices["restore_token"]), (2, "token-1"))

    def test_desktop_side_close_deletes_the_token_file(self):
        fake = FakePortal()
        backend = backend_for(fake, restore_token_file=self.path)
        backend.connect()
        closed = [cb for (i, s, p, cb) in fake.subscriptions.values() if s == "Closed"][0]
        closed()
        self.assertFalse(os.path.lexists(self.path))
        # A restarted connector has nothing to restore either.
        fake = FakePortal()
        backend_for(fake, restore_token_file=self.path).connect()
        devices = plain([c for c in fake.calls if c[1] == "SelectDevices"][0][3][-1])
        self.assertEqual(devices["persist_mode"], 2)
        self.assertNotIn("restore_token", devices)

    def test_token_file_that_cannot_be_deleted_is_not_reused(self):
        fake = FakePortal()
        backend = backend_for(fake, restore_token_file=self.path)
        backend.connect()
        closed = [cb for (i, s, p, cb) in fake.subscriptions.values() if s == "Closed"][0]
        denied = PermissionError(13, "Permission denied")
        with mock.patch.object(portal_module.os, "unlink", side_effect=denied), \
                self.assertLogs(portal_module.log, "WARNING"):
            closed()  # a signal handler must not raise
        self.assertTrue(os.path.exists(self.path))
        self.assertEqual(backend.describe()["restore_token"], "forgotten because the desktop ended sharing")
        fake.calls.clear()
        backend.connect()
        devices = plain([c for c in fake.calls if c[1] == "SelectDevices"][0][3][-1])
        self.assertNotIn("restore_token", devices)
        # The new grant replaced the file, so later connects may use it again.
        self.assertEqual(backend.describe()["restore_token"], "stored in the configured file")

    def test_unsafe_token_file_is_rejected(self):
        with open(self.path, "w") as handle:
            handle.write("token-0\n")
        os.chmod(self.path, 0o644)
        with self.assertRaisesRegex(DesktopError, "0600"):
            TokenStore(self.path).load()
        os.chmod(self.path, 0o600)
        self.assertEqual(TokenStore(self.path).load(), "token-0")
        link = os.path.join(self.tmp.name, "link")
        os.symlink(self.path, link)
        with self.assertRaises(DesktopError):
            TokenStore(link).load()
        self.assertIsNone(TokenStore(os.path.join(self.tmp.name, "absent")).load())


class InputTests(unittest.TestCase):
    def setUp(self):
        self.fake = FakePortal()
        self.backend = backend_for(self.fake)
        self.backend.connect()
        self.fake.calls.clear()
        self.output = self.backend.outputs()[0]

    def notify(self):
        return [(c[1],) + tuple(c[3][2:]) for c in self.fake.calls if c[1].startswith("Notify")]

    def test_wire_signatures(self):
        self.backend.pointer_move_abs(self.output, 12.5, 3000.0)
        self.backend.button("right", True)
        self.backend.button("right", False)
        self.backend.scroll(-1, 2)
        self.backend.key_chord(parse_chord(["ctrl", "l"]))
        self.backend.type_text("λ")
        self.assertEqual(self.notify(), [
            ("NotifyPointerMotionAbsolute", 41, 12.5, 1439.0),
            ("NotifyPointerButton", 0x111, 1), ("NotifyPointerButton", 0x111, 0),
            ("NotifyPointerAxisDiscrete", 0, 2), ("NotifyPointerAxisDiscrete", 1, -1),
            ("NotifyKeyboardKeysym", 0xFFE3, 1), ("NotifyKeyboardKeysym", 0x6C, 1),
            ("NotifyKeyboardKeysym", 0x6C, 0), ("NotifyKeyboardKeysym", 0xFFE3, 0),
            ("NotifyKeyboardKeysym", 0x010003BB, 1), ("NotifyKeyboardKeysym", 0x010003BB, 0),
        ])
        session = self.fake.calls[0][3][0]
        self.assertTrue(session.startswith("/org/freedesktop/portal/desktop/session/"))

    def test_failure_closes_the_session_without_leaking_details(self):
        self.fake.fail_notify = True
        with self.assertRaises(DesktopError) as caught:
            self.backend.scroll(0, 1)
        self.assertNotIn("private", str(caught.exception))
        self.assertFalse(self.backend.connected)


class CaptureTests(unittest.TestCase):
    def setUp(self):
        self.fake = FakePortal()
        self.backend = backend_for(self.fake)
        self.backend.connect()
        self.output = self.backend.outputs()[0]

    def test_each_capture_uses_a_fresh_remote_and_closes_it(self):
        for _ in range(2):
            shot = self.backend.capture(self.output, 1280, "png")
            self.assertEqual((shot.mime, shot.width, shot.height), ("image/png", 1280, 720))
            with self.assertRaises(OSError):
                os.fstat(self.fake.fds[-1])
        pipeline = self.fake.pipelines[-1]
        self.assertIn("width=1280,height=720", pipeline.description)
        self.assertIn("pngenc snapshot=true", pipeline.description)
        self.assertEqual(pipeline.source.values["target-object"], "99")
        self.assertEqual(pipeline.source.values["keepalive-time"], 1000)
        self.assertEqual(pipeline.states[-1], 1)
        self.assertEqual(sum(1 for c in self.fake.calls if c[1] == "OpenPipeWireRemote"), 2)

    def test_node_id_fallback_region_crop_and_jpeg(self):
        second = self.backend.outputs()[1]
        self.fake.sample = cu_support.make_jpeg(320, 180)
        shot = self.backend.capture(second, 1280, "jpeg", region=Rect(320, 180, 320, 180), jpeg_quality=70)
        self.assertEqual(shot.mime, "image/jpeg")
        pipeline = self.fake.pipelines[-1]
        self.assertEqual(pipeline.source.values["path"], "42")
        self.assertNotIn("target-object", pipeline.source.values)
        self.assertIn("width=1280,height=720", pipeline.description)
        self.assertIn("videocrop left=320 right=640 top=180 bottom=360", pipeline.description)
        self.assertIn("jpegenc quality=70", pipeline.description)

    def test_a_changed_monitor_pixel_size_closes_the_session(self):
        self.backend.capture(self.output, 1280, "png")
        self.backend.capture(self.output, 1280, "png")  # same size: fine
        generation = self.backend.generation
        self.fake.calls.clear()
        self.fake.stream_pixel_size = (1920, 1080)  # e.g. the monitor's mode was changed
        with self.assertRaisesRegex(DesktopError, "changed from 2560x1440 to 1920x1080.*desktop_connect"):
            self.backend.capture(self.output, 1280, "png")
        self.assertFalse(self.backend.connected)
        self.assertGreater(self.backend.generation, generation)
        self.assertIn((SESSION, "Close"), self.fake.methods())
        self.assertEqual(self.fake.pipelines[-1].states[-1], 1)
        with self.assertRaises(OSError):
            os.fstat(self.fake.fds[-1])
        # A new session starts with the new size as its reference.
        self.backend.connect()
        shot = self.backend.capture(self.backend.outputs()[0], 1280, "png")
        self.assertEqual(shot.mime, "image/png")

    def test_each_stream_has_its_own_reference_size_and_missing_caps_are_tolerated(self):
        first, second = self.backend.outputs()
        self.backend.capture(first, 1280, "png")
        self.fake.stream_pixel_size = (1280, 720)
        self.backend.capture(second, 1280, "png")  # the second monitor's own first capture
        self.fake.stream_pixel_size = None
        self.backend.capture(first, 1280, "png")
        self.assertTrue(self.backend.connected)

    def test_no_frame_releases_pipeline_and_descriptor(self):
        self.fake.sample = None
        with self.assertRaisesRegex(DesktopError, "no desktop frame"):
            self.backend.capture(self.output, 1280, "png")
        self.assertEqual(self.fake.pipelines[-1].states[-1], 1)
        with self.assertRaises(OSError):
            os.fstat(self.fake.fds[-1])


class ProbeTests(unittest.TestCase):
    def test_probe_reads_only_the_version_property(self):
        fake = FakePortal()
        result = portal_module.probe_remote_desktop({}, loader=lambda: fake.gi, bus_opener=lambda gi, env: fake)
        self.assertEqual(result, {"available": True, "version": 2})
        self.assertEqual(fake.methods(), [(PROPERTIES, "Get")])

    def test_probe_reports_why_the_portal_is_unusable(self):
        fake = FakePortal(properties={})
        result = portal_module.probe_remote_desktop({}, loader=lambda: fake.gi, bus_opener=lambda gi, env: fake)
        self.assertFalse(result["available"])
        self.assertIn(REMOTE, result["reason"])

        def no_gi():
            raise DesktopError("the portal backend needs the system PyGObject")

        self.assertIn("PyGObject", portal_module.probe_remote_desktop({}, loader=no_gi)["reason"])
        with tempfile.TemporaryDirectory() as empty:
            result = portal_module.probe_remote_desktop({"XDG_RUNTIME_DIR": empty}, loader=lambda: fake.gi)
        self.assertIn("no D-Bus session bus", result["reason"])

    def test_portal_module_does_not_import_gi(self):
        self.assertNotIn("gi", sys.modules)


if __name__ == "__main__":
    unittest.main()
