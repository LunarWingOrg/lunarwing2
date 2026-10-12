"""WI#376: environment-mode configuration and local HTTP forwarding smoke test."""

import contextlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import threading
import unittest
from unittest import mock
import urllib.request


SPEC = importlib.util.spec_from_file_location(
    "lunarwing_proxy", Path(__file__).resolve().parents[1] / "lunarwing-proxy.py"
)
proxy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(proxy)


class ProxyConfigurationTests(unittest.TestCase):
    def test_env_mode_preserves_url_as_literal_data(self):
        for value in (
            "https://fixture.invalid/$(not-a-command${IFS}argument)",
            "https://fixture.invalid/`not-a-command`",
            'https://fixture.invalid/";not-a-command;#',
            "https://fixture.invalid/prefix?a=one&b=two",
        ):
            with (
                self.subTest(value=value),
                mock.patch.dict(os.environ, {"TENSORZERO_URL": value}, clear=True),
            ):
                self.assertEqual(proxy.parse_args(["--tensorzero-env"]).tensorzero, value)

    def test_env_mode_requires_nonempty_configuration_before_binding(self):
        for values in ({}, {"TENSORZERO_URL": ""}, {"TENSORZERO_URL": "  "}):
            with (
                self.subTest(values=values),
                mock.patch.dict(os.environ, values, clear=True),
                mock.patch("sys.argv", ["lunarwing-proxy.py", "--tensorzero-env"]),
                mock.patch.object(proxy, "ThreadedServer") as server,
                contextlib.redirect_stderr(io.StringIO()) as errors,
            ):
                with self.assertRaises(SystemExit) as raised:
                    proxy.main()
                self.assertEqual(raised.exception.code, 2)
                self.assertIn("TENSORZERO_URL is required", errors.getvalue())
                server.assert_not_called()

    def test_explicit_cli_url_retains_precedence_outside_env_mode(self):
        with mock.patch.dict(os.environ, {"TENSORZERO_URL": "https://env.invalid"}):
            args = proxy.parse_args(["--tensorzero", "https://cli.invalid"])
        self.assertEqual(args.tensorzero, "https://cli.invalid")

    def test_cli_and_env_modes_are_mutually_exclusive(self):
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                proxy.parse_args(["--tensorzero-env", "--tensorzero", "https://cli.invalid"])
        self.assertEqual(raised.exception.code, 2)

    def test_standalone_default_is_unchanged(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(proxy.parse_args([]).tensorzero, proxy.TENSORZERO_URL)

    def test_http_request_reaches_env_selected_upstream(self):
        paths = []
        response = {"object": "list", "data": [{"id": "fixture-model"}]}

        class Upstream(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                paths.append(self.path)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps(response).encode())

            def log_message(self, *_args):
                pass

        class QuietProxy(proxy.LunarWingProxyHandler):
            def log_message(self, *_args):
                pass

        # OS-assigned ports and in-process servers keep this hermetic and avoid
        # races between probing a free port and starting a subprocess on it.
        with (
            http.server.HTTPServer(("127.0.0.1", 0), Upstream) as upstream,
            proxy.ThreadedServer(("127.0.0.1", 0), QuietProxy) as listener,
        ):
            url = f"http://127.0.0.1:{upstream.server_port}/fixture-prefix"
            with mock.patch.dict(os.environ, {"TENSORZERO_URL": url}, clear=True):
                args = proxy.parse_args(["--tensorzero-env"])
                with mock.patch.object(proxy, "TENSORZERO_URL", args.tensorzero):
                    threads = [
                        threading.Thread(target=server.serve_forever, daemon=True)
                        for server in (upstream, listener)
                    ]
                    for thread in threads:
                        thread.start()
                    try:
                        with urllib.request.urlopen(
                            f"http://127.0.0.1:{listener.server_address[1]}/models", timeout=5
                        ) as result:
                            self.assertEqual(json.load(result), response)
                        self.assertEqual(paths, ["/fixture-prefix/openai/v1/models"])
                    finally:
                        listener.shutdown()
                        upstream.shutdown()
                        for thread in threads:
                            thread.join(timeout=5)
                            self.assertFalse(thread.is_alive())


if __name__ == "__main__":
    unittest.main()
