"""WAYLAND_DISPLAY and D-Bus session bus discovery with a temporary runtime dir."""

import os
import socket
import tempfile
import unittest

import cu_support  # noqa: F401

from lunarwing_computer import session_env
from lunarwing_computer.control import DesktopError


class RuntimeDirTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cu-rt-")
        self.dir = self.tmp.name
        self.sockets = []

    def tearDown(self):
        for sock in self.sockets:
            sock.close()
        self.tmp.cleanup()

    def make_socket(self, name):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.bind(os.path.join(self.dir, name))
        self.sockets.append(sock)
        return os.path.join(self.dir, name)

    def make_file(self, name):
        with open(os.path.join(self.dir, name), "w") as handle:
            handle.write("x")


class WaylandDiscoveryTests(RuntimeDirTestCase):
    def test_single_socket_is_used_and_lookalikes_are_ignored(self):
        path = self.make_socket("wayland-1")
        self.make_file("wayland-1.lock")
        self.make_socket("wayland-1-swww-daemon..sock")
        self.make_file("wayland-2")  # a regular file, not a socket
        env = {"XDG_RUNTIME_DIR": self.dir}
        self.assertEqual(session_env.resolve_wayland_socket(env), path)
        self.assertEqual(session_env.wayland_socket_candidates(self.dir), ["wayland-1"])

    def test_zero_or_several_sockets_are_errors(self):
        env = {"XDG_RUNTIME_DIR": self.dir}
        with self.assertRaisesRegex(DesktopError, "no wayland-N socket"):
            session_env.resolve_wayland_socket(env)
        self.make_socket("wayland-0")
        self.make_socket("wayland-3")
        with self.assertRaisesRegex(DesktopError, "wayland-0, wayland-3"):
            session_env.resolve_wayland_socket(env)

    def test_explicit_wayland_display_relative_and_absolute(self):
        relative = self.make_socket("wayland-7")
        self.make_socket("wayland-8")
        self.assertEqual(session_env.resolve_wayland_socket({"XDG_RUNTIME_DIR": self.dir, "WAYLAND_DISPLAY": "wayland-7"}), relative)
        absolute = self.make_socket("custom.sock")
        self.assertEqual(session_env.resolve_wayland_socket({"WAYLAND_DISPLAY": absolute}), absolute)
        with self.assertRaisesRegex(DesktopError, "not a Wayland socket"):
            session_env.resolve_wayland_socket({"XDG_RUNTIME_DIR": self.dir, "WAYLAND_DISPLAY": "wayland-9"})

    def test_runtime_dir_falls_back_to_run_user_uid(self):
        self.assertEqual(session_env.runtime_dir({}), f"/run/user/{os.getuid()}")
        self.assertEqual(session_env.runtime_dir({"XDG_RUNTIME_DIR": "/x"}), "/x")

    def test_child_env_points_grim_at_the_same_socket(self):
        env = session_env.wayland_child_env({"PATH": "/usr/bin"}, "/run/user/5/wayland-2")
        self.assertEqual(env, {"PATH": "/usr/bin", "XDG_RUNTIME_DIR": "/run/user/5", "WAYLAND_DISPLAY": "wayland-2"})


class DbusDiscoveryTests(RuntimeDirTestCase):
    def test_explicit_address_wins(self):
        self.make_socket("bus")
        env = {"XDG_RUNTIME_DIR": self.dir, "DBUS_SESSION_BUS_ADDRESS": "unix:path=/elsewhere"}
        self.assertEqual(session_env.discover_dbus_address(env), "unix:path=/elsewhere")

    def test_runtime_dir_bus_socket(self):
        env = {"XDG_RUNTIME_DIR": self.dir}
        self.assertIsNone(session_env.discover_dbus_address(env))
        self.make_file("bus")
        self.assertIsNone(session_env.discover_dbus_address(env))
        os.unlink(os.path.join(self.dir, "bus"))
        path = self.make_socket("bus")
        self.assertEqual(session_env.discover_dbus_address(env), "unix:path=" + session_env.dbus_escape(path))

    def test_address_escaping(self):
        self.assertEqual(session_env.dbus_escape("/run/user/1000/bus"), "/run/user/1000/bus")
        self.assertEqual(session_env.dbus_escape("/tmp/a b,c=d"), "/tmp/a%20b%2cc%3dd")


if __name__ == "__main__":
    unittest.main()
