"""Portal request/cleanup tests without opening or controlling a real desktop."""

import os
import struct
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

from lunarwing_computer.control import DesktopError
from lunarwing_computer.portal import CAST, REMOTE, REQUEST, SESSION, Portal


class Variant:
    def __init__(self, signature, values):
        self.signature = signature
        self.values = values

    def unpack(self):
        return self.values


class PortalTests(unittest.TestCase):
    def setUp(self):
        self.portal = Portal.__new__(Portal)
        self.portal.session = None
        self.portal.subscription = None
        self.portal.stream = None
        self.portal.size = None
        self.portal.serial = None
        self.portal.active = False
        self.portal.pump = Mock()
        self.loop = Mock()
        self.portal.GLib = SimpleNamespace(
            Variant=Variant, MainLoop=lambda: self.loop,
            timeout_add=Mock(return_value=42), source_remove=Mock(),
        )
        self.portal.Gio = SimpleNamespace(DBusSignalFlags=SimpleNamespace(NONE=0))
        self.portal.bus = Mock()
        self.portal.bus.get_unique_name.return_value = ":1.23"
        self.portal.bus.signal_subscribe.return_value = 7

    def test_subscribes_before_call_and_accepts_early_response(self):
        def call(interface, method, signature, values):
            self.portal.bus.signal_subscribe.assert_called_once()
            args = self.portal.bus.signal_subscribe.call_args.args
            callback, request_path = args[6], args[3]
            callback(None, None, None, None, None, Variant("(ua{sv})", (0, {"session_handle": "/session"})), None)
            self.assertEqual(values[-1]["handle_token"].values, request_path.rsplit("/", 1)[1])
            return Variant("(o)", (request_path,))
        self.portal.call = call
        result = self.portal.request(REMOTE, "CreateSession", "(a{sv})", (), {})
        self.assertEqual(result, {"session_handle": "/session"})
        self.loop.run.assert_not_called()
        self.portal.bus.signal_unsubscribe.assert_called_once_with(7)
        self.portal.GLib.source_remove.assert_called_once_with(42)

    def test_timeout_closes_pending_request(self):
        closed = []

        def call(interface, method, signature, values, **kwargs):
            if method == "Close":
                closed.append((interface, kwargs["path"]))
                return Variant("()", ())
            return Variant("(o)", (self.portal.bus.signal_subscribe.call_args.args[3],))

        self.portal.call = call
        self.loop.run.side_effect = lambda: self.portal.GLib.timeout_add.call_args.args[1]()
        with self.assertRaisesRegex(DesktopError, "timed out"):
            self.portal.request(REMOTE, "Start", "(osa{sv})", ("/session", ""), {})
        self.assertEqual(closed[0][0], REQUEST)
        self.portal.bus.signal_unsubscribe.assert_called_once_with(7)
        self.portal.GLib.source_remove.assert_not_called()

    def test_start_uses_shared_remote_session_and_checks_grants(self):
        for devices in (1, 3):
            with self.subTest(devices=devices):
                self.portal.request = Mock(side_effect=[
                    {"session_handle": "/session"}, {}, {},
                    {"devices": devices, "streams": [(17, {"size": (1600, 900)})]},
                ])
                self.portal.call = Mock()
                if devices == 1:
                    with self.assertRaisesRegex(DesktopError, "keyboard and pointer"):
                        self.portal.start()
                    self.assertFalse(self.portal.active)
                    self.portal.call.assert_called_with(SESSION, "Close", "()", (), path="/session")
                else:
                    self.portal.start()
                    self.assertTrue(self.portal.active)
                    self.assertEqual(self.portal.stream, 17)
                methods = [(c.args[0], c.args[1]) for c in self.portal.request.call_args_list]
                self.assertEqual(methods, [(REMOTE, "CreateSession"), (REMOTE, "SelectDevices"),
                                           (CAST, "SelectSources"), (REMOTE, "Start")])

    def test_remote_close_invalidates_input(self):
        self.portal.session = "/session"
        self.portal.active = True
        self.portal.subscription = 7
        self.portal.call = Mock()
        self.portal.closed()
        self.assertFalse(self.portal.active)
        with self.assertRaises(DesktopError):
            self.portal.key(65, True)
        self.portal.call.assert_not_called()
        self.portal.bus.signal_unsubscribe.assert_called_once_with(7)

    def test_input_wire_signatures_and_fail_closed(self):
        self.portal.session = "/session"
        self.portal.stream = 17
        self.portal.active = True
        self.portal.call = Mock()
        self.portal.move(12.5, 34.0)
        self.portal.call.assert_called_with(REMOTE, "NotifyPointerMotionAbsolute", "(oa{sv}udd)",
                                            ("/session", {}, 17, 12.5, 34.0))
        self.portal.button("right", True)
        self.portal.call.assert_called_with(REMOTE, "NotifyPointerButton", "(oa{sv}iu)",
                                            ("/session", {}, 273, 1))
        self.portal.key(65, False)
        self.portal.call.assert_called_with(REMOTE, "NotifyKeyboardKeysym", "(oa{sv}iu)",
                                            ("/session", {}, 65, 0))
        self.portal.call.side_effect = RuntimeError("private D-Bus diagnostic")
        with self.assertRaisesRegex(DesktopError, "session closed"):
            self.portal.scroll("vertical", 2)
        self.assertFalse(self.portal.active)
        self.assertIsNone(self.portal.session)

    def capture_fixture(self):
        self.portal.session = "/session"
        self.portal.stream = 17
        self.portal.size = (2560, 1440)
        self.portal.active = True
        png = b"\x89PNG\r\n\x1a\n" + b"\0" * 8 + struct.pack(">II", 1600, 900)
        buffer = Mock()
        buffer.get_size.return_value = len(png)
        buffer.extract_dup.return_value = png
        sample = Mock()
        sample.get_buffer.return_value = buffer
        sink = Mock()
        sink.try_pull_sample.return_value = sample
        source = Mock()
        pipeline = Mock()
        pipeline.get_by_name.side_effect = lambda name: source if name == "source" else sink
        self.portal.Gst = SimpleNamespace(
            parse_launch=Mock(return_value=pipeline), SECOND=1000000000,
            State=SimpleNamespace(PLAYING=1, NULL=0), StateChangeReturn=SimpleNamespace(FAILURE=-1),
        )
        self.portal.GLib.VariantType = SimpleNamespace(new=lambda signature: signature)
        self.portal.Gio.DBusCallFlags = SimpleNamespace(NONE=0)
        descriptors = []

        def open_remote(*_args):
            fd = os.open(os.devnull, os.O_RDONLY)
            descriptors.append(fd)
            return Variant("(h)", (0,)), SimpleNamespace(get=lambda _index: fd)

        self.portal.bus.call_with_unix_fd_list_sync.side_effect = open_remote
        return pipeline, sink, descriptors

    def test_each_capture_uses_fresh_pipewire_remote_and_closes_it(self):
        pipeline, _sink, descriptors = self.capture_fixture()
        for _ in range(2):
            frame = self.portal.capture()
            self.assertEqual((frame.width, frame.height), (1600, 900))
            self.assertEqual((frame.logical_width, frame.logical_height), (2560, 1440))
            with self.assertRaises(OSError):
                os.fstat(descriptors[-1])
        self.assertEqual(self.portal.bus.call_with_unix_fd_list_sync.call_count, 2)
        pipeline.set_state.assert_called_with(0)

    def test_capture_timeout_releases_pipeline_and_descriptor(self):
        pipeline, sink, descriptors = self.capture_fixture()
        sink.try_pull_sample.return_value = None
        with self.assertRaisesRegex(DesktopError, "No desktop frame"):
            self.portal.capture()
        pipeline.set_state.assert_called_with(0)
        with self.assertRaises(OSError):
            os.fstat(descriptors[-1])


if __name__ == "__main__":
    unittest.main()
