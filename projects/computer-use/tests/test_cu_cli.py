"""The command line and the stdio process, without PyGObject or a display."""

import json
import os
import subprocess
import sys
import tempfile
import unittest

import cu_support  # noqa: F401
from cu_support import PROJECT

from lunarwing_computer.__main__ import build_parser

LAUNCHER = os.path.join(PROJECT, "computer-use.py")
# Run the launcher with gi made unimportable, as on a CI image without PyGObject.
NO_GI = (
    "import runpy, sys; sys.modules['gi'] = None; sys.argv = [sys.argv[1]] + sys.argv[2:]; "
    "runpy.run_path(sys.argv[0], run_name='__main__')"
)


def isolated_env(runtime_dir):
    # No WAYLAND_DISPLAY, DBUS_SESSION_BUS_ADDRESS or DISPLAY, like LunarWing's scrubbed child
    # environment, and an empty runtime dir so nothing can reach a real session.
    return {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": runtime_dir, "XDG_RUNTIME_DIR": runtime_dir}


class CliTests(unittest.TestCase):
    def test_parser_defaults_and_bounds(self):
        args = build_parser().parse_args([])
        self.assertEqual((args.backend, args.max_edge, args.format, args.jpeg_quality, args.log_level),
                         ("auto", 1280, "png", 80, "warning"))
        for bad in (["--max-edge", "10"], ["--jpeg-quality", "100"], ["--backend", "x11"], ["--format", "gif"]):
            with self.subTest(bad=bad), self.assertRaises(SystemExit):
                build_parser().parse_args(bad)

    def test_help_works_without_gi_or_display(self):
        with tempfile.TemporaryDirectory() as runtime:
            completed = subprocess.run([sys.executable, "-I", "-c", NO_GI, LAUNCHER, "--help"],
                                       env=isolated_env(runtime), capture_output=True, timeout=30)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn(b"--restore-token-file", completed.stdout)
        self.assertIn(b"--max-edge", completed.stdout)

    def test_stdio_session_without_a_display(self):
        messages = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize",
             "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "desktop_status", "arguments": {}}},
            {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "desktop_connect", "arguments": {}}},
        ]
        data = "".join(json.dumps(m) + "\n" for m in messages).encode()
        with tempfile.TemporaryDirectory() as runtime:
            completed = subprocess.run([sys.executable, "-I", "-c", NO_GI, LAUNCHER, "--log-level", "info"],
                                       input=data, env=isolated_env(runtime), capture_output=True, timeout=30)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        lines = completed.stdout.decode("ascii").splitlines()
        responses = [json.loads(line) for line in lines]  # stdout carries nothing but JSON-RPC
        self.assertEqual([r["id"] for r in responses], [1, 2, 3, 4])
        self.assertEqual(responses[0]["result"]["protocolVersion"], "2024-11-05")
        self.assertEqual(len(responses[1]["result"]["tools"]), 12)
        status = json.loads(responses[2]["result"]["content"][0]["text"])
        self.assertFalse(status["connected"])
        self.assertIn("no wayland-N socket", status["probe"]["wayland"]["error"])
        self.assertIn("PyGObject", status["probe"]["portal"]["reason"])
        connect = responses[3]["result"]
        self.assertTrue(connect["isError"])
        self.assertIn("no usable desktop backend", connect["content"][0]["text"])
        self.assertIn(b"desktop_connect failed", completed.stderr)


if __name__ == "__main__":
    unittest.main()
