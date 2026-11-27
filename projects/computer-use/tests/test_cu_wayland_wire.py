"""Wayland wire encoder/decoder and connection handling against a fake compositor."""

import collections
import os
import socket
import struct
import sys
import unittest

import cu_support  # noqa: F401
from cu_support import FakeCompositor, FakeOutput

from lunarwing_computer import wayland_wire
from lunarwing_computer.wayland_wire import Connection, ProtocolError, WaylandError

N = "<" if sys.byteorder == "little" else ">"


class EncoderTests(unittest.TestCase):
    def test_header_and_scalar_arguments(self):
        data, fds = wayland_wire.encode_message(5, 3, "iuf", (-2, 7, 1.5))
        self.assertEqual(fds, [])
        self.assertEqual(data, struct.pack(N + "IIiIi", 5, (20 << 16) | 3, -2, 7, 384))

    def test_strings_are_nul_terminated_and_padded(self):
        body, _ = wayland_wire.encode_args("s", ("wl_output",))
        self.assertEqual(body, struct.pack(N + "I", 10) + b"wl_output\0\0\0")
        body, _ = wayland_wire.encode_args("s", ("abc",))
        self.assertEqual(body, struct.pack(N + "I", 4) + b"abc\0")
        body, _ = wayland_wire.encode_args("s", (None,))
        self.assertEqual(body, struct.pack(N + "I", 0))

    def test_objects_arrays_and_fds(self):
        body, fds = wayland_wire.encode_args("onah", (None, 9, b"\x01\x02\x03", 42))
        self.assertEqual(body, struct.pack(N + "III", 0, 9, 3) + b"\x01\x02\x03\0")
        self.assertEqual(fds, [42])

    def test_untyped_new_id_for_bind(self):
        body, _ = wayland_wire.encode_args("usun", (12, "wl_seat", 5, 20))
        self.assertEqual(body, struct.pack(N + "II", 12, 8) + b"wl_seat\0" + struct.pack(N + "II", 5, 20))

    def test_rejects_bad_input(self):
        for signature, args in [("u", (-1,)), ("u", (1 << 32,)), ("x", (1,)), ("uu", (1,))]:
            with self.subTest(signature=signature), self.assertRaises(ValueError):
                wayland_wire.encode_args(signature, args)
        with self.assertRaises(ValueError):
            wayland_wire.encode_message(1, 0, "s", ("x" * 5000,))

    def test_fixed_conversion(self):
        self.assertEqual(wayland_wire.fixed_from_float(15.0), 3840)
        self.assertEqual(wayland_wire.fixed_from_float(-15.0), -3840)
        self.assertEqual(wayland_wire.fixed_to_float(-3840), -15.0)

    def test_decoder(self):
        body = (struct.pack(N + "ii", -5, 1280) + struct.pack(N + "I", 6) + b"DP-1x\0\0\0"
                + struct.pack(N + "I", 0) + struct.pack(N + "i", 640) + struct.pack(N + "I", 2) + b"\x07\x08\0\0")
        values = wayland_wire.decode_args("iissfa", body, collections.deque())
        self.assertEqual(values, (-5, 1280, "DP-1x", None, 2.5, b"\x07\x08"))
        with self.assertRaises(WaylandError):
            wayland_wire.decode_args("ii", struct.pack(N + "i", 1), collections.deque())
        with self.assertRaises(WaylandError):
            wayland_wire.decode_args("h", b"", collections.deque())


class ConnectionTests(unittest.TestCase):
    def setUp(self):
        self.fake = FakeCompositor([
            FakeOutput("DP-3", 1280, 0, 2560, 1440, 2560, 1440),
            FakeOutput("HDMI-A-1", 0, 720, 1280, 720, 1920, 1080, scale=2),
        ])
        self.conn = Connection(self.fake.client_socket, timeout=2.0)

    def tearDown(self):
        self.conn.close()
        self.fake.close()

    def registry(self):
        seen = {}

        def handler(event, args):
            if event == "global":
                seen[args[0]] = (args[1], args[2])

        registry = self.conn.get_registry(handler)
        self.conn.roundtrip()
        return registry, seen

    def test_registry_and_roundtrip(self):
        registry, seen = self.registry()
        self.assertEqual(registry.id, 2)
        interfaces = sorted(i for i, _v in seen.values())
        self.assertIn("zwlr_virtual_pointer_manager_v1", interfaces)
        self.assertEqual(interfaces.count("wl_output"), 2)
        self.assertEqual(self.fake.requests_for("wl_display", "get_registry"), [("wl_display", "get_registry", 2)])
        self.assertEqual(len(self.fake.requests_for("wl_display", "sync")), 1)

    def test_bind_and_output_events(self):
        registry, seen = self.registry()
        events = []
        outputs = sorted(name for name, (iface, _v) in seen.items() if iface == "wl_output")
        manager_name = next(name for name, (iface, _v) in seen.items() if iface == "zxdg_output_manager_v1")
        manager = self.conn.bind(registry, manager_name, "zxdg_output_manager_v1", 3)
        for name in outputs:
            output = self.conn.bind(registry, name, "wl_output", 4)
            output.handler = lambda event, args, n=name: events.append((n, "wl", event, args))
            xdg = self.conn.new_proxy("zxdg_output_v1", 3)
            xdg.handler = lambda event, args, n=name: events.append((n, "xdg", event, args))
            manager.request(1, "no", xdg, output)
        self.conn.roundtrip()
        first = [e[2:] for e in events if e[0] == outputs[0]]
        self.assertIn(("geometry", (1280, 0, 600, 340, 0, "Make", "Model", 0)), first)
        self.assertIn(("mode", (3, 2560, 1440, 60000)), first)
        self.assertIn(("scale", (1,)), first)
        self.assertIn(("name", ("DP-3",)), first)
        self.assertIn(("done", ()), first)
        self.assertIn(("logical_position", (1280, 0)), first)
        self.assertIn(("logical_size", (2560, 1440)), first)
        second = [e[2:] for e in events if e[0] == outputs[1]]
        self.assertIn(("logical_size", (1280, 720)), second)
        self.assertIn(("scale", (2,)), second)
        bind = self.fake.requests_for("wl_registry", "bind")
        self.assertEqual([(b[3], b[4]) for b in bind], [("zxdg_output_manager_v1", 3), ("wl_output", 4), ("wl_output", 4)])

    def test_delete_id_frees_ids_for_reuse(self):
        self.registry()
        callback_id = self.fake.requests_for("wl_display", "sync")[0][2]
        self.assertNotIn(callback_id, self.conn.objects)
        self.assertEqual(self.conn.allocate_id(), callback_id)
        next_id = self.conn.allocate_id()
        self.assertGreater(next_id, callback_id)

    def test_fake_compositor_rejects_a_gap_in_new_ids_like_libwayland(self):
        self.registry()
        self.conn.allocate_id()  # the freed callback id: reusing it is fine
        self.conn.allocate_id()  # a fresh id that is never sent leaves a gap
        with self.assertRaisesRegex(ProtocolError, "invalid new id"):
            self.conn.roundtrip()

    def test_display_error_raises_and_poisons_the_connection(self):
        registry, seen = self.registry()
        manager_name = next(name for name, (iface, _v) in seen.items() if iface == "zwp_virtual_keyboard_manager_v1")
        seat_name = next(name for name, (iface, _v) in seen.items() if iface == "wl_seat")
        manager = self.conn.bind(registry, manager_name, "zwp_virtual_keyboard_manager_v1", 1)
        seat = self.conn.bind(registry, seat_name, "wl_seat", 5)
        keyboard = self.conn.new_proxy("zwp_virtual_keyboard_v1", 1)
        manager.request(0, "on", seat, keyboard)
        keyboard.request(1, "uuu", 0, 30, 1)  # key before keymap: protocol error no_keymap
        with self.assertRaises(ProtocolError) as caught:
            self.conn.roundtrip()
        self.assertEqual(caught.exception.interface, "zwp_virtual_keyboard_v1")
        self.assertEqual(caught.exception.code, 0)
        self.assertIn("No keymap", str(caught.exception))
        with self.assertRaises(ProtocolError):
            keyboard.request(1, "uuu", 0, 30, 0)

    def test_compositor_hangup_is_a_wayland_error(self):
        self.registry()
        self.fake.server_socket.shutdown(socket.SHUT_RDWR)
        with self.assertRaises(WaylandError):
            self.conn.roundtrip()

    def test_fd_is_sent_with_scm_rights(self):
        registry, seen = self.registry()
        manager_name = next(name for name, (iface, _v) in seen.items() if iface == "zwp_virtual_keyboard_manager_v1")
        seat_name = next(name for name, (iface, _v) in seen.items() if iface == "wl_seat")
        manager = self.conn.bind(registry, manager_name, "zwp_virtual_keyboard_manager_v1", 1)
        seat = self.conn.bind(registry, seat_name, "wl_seat", 5)
        keyboard = self.conn.new_proxy("zwp_virtual_keyboard_v1", 1)
        manager.request(0, "on", seat, keyboard)
        fd = os.memfd_create("test")
        os.write(fd, b"keymap-bytes\0")
        try:
            keyboard.request(0, "uhu", 1, fd, 13)
        finally:
            os.close(fd)
        self.conn.roundtrip()
        self.assertEqual(self.fake.keymaps, [(1, 13, b"keymap-bytes\0")])


class TimeoutTests(unittest.TestCase):
    def test_silent_compositor_times_out(self):
        client, server = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            conn = Connection(client, timeout=0.2)
            with self.assertRaisesRegex(WaylandError, "did not"):
                conn.roundtrip()
        finally:
            client.close()
            server.close()

    def test_connect_to_missing_socket(self):
        with self.assertRaises(WaylandError):
            Connection.connect("/nonexistent/wayland-77", timeout=0.2)


if __name__ == "__main__":
    unittest.main()
