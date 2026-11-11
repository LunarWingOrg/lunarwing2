#!/usr/bin/env python3

import importlib.util
import json
import os
import tempfile
import unittest
from unittest import mock
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("runtime_smoke", Path(__file__).with_name("runtime_smoke.py"))
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class RuntimeSmokeTests(unittest.TestCase):
    def test_fake_model_supports_streaming_and_plain_responses(self):
        text, content_type = MODULE.completion("model", True)
        self.assertEqual(content_type, "text/event-stream")
        self.assertIn("LunarDepth smoke OK", text)
        self.assertTrue(text.endswith("data: [DONE]\n\n"))
        text, content_type = MODULE.completion("model", False)
        self.assertEqual(content_type, "application/json")
        self.assertEqual(json.loads(text)["choices"][0]["message"]["content"], "LunarDepth smoke OK")

    def test_fresh_policy_requires_actual_events(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            session = home / "sessions" / "test.jsonl"
            session.parent.mkdir(parents=True)
            events = [
                {"type": "permission/preset", "data": {"preset": "workspace-write"}},
                {"type": "sandbox/mode", "data": {"mode": "workspace-write"}},
                {"type": "approval/policy", "data": {"policy": "ask"}},
            ]
            session.write_text("\n".join(json.dumps(event) for event in events), encoding="utf-8")
            MODULE.assert_fresh_policy(home)
            events[0]["data"]["preset"] = "danger-full-access"
            session.write_text("\n".join(json.dumps(event) for event in events), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "no fresh task session"):
                MODULE.assert_fresh_policy(home)

    def test_bridge_smoke_uses_the_registry_port(self):
        with mock.patch.dict(os.environ, {"WS_PORT": "52000"}):
            self.assertEqual(MODULE.bridge_ws_url(), "ws://127.0.0.1:52000/ws/agent")
        with mock.patch.dict(os.environ, {"SMOKE_BRIDGE_WS_URL": "ws://127.0.0.1:52999/ws/agent"}):
            self.assertEqual(MODULE.bridge_ws_url(), "ws://127.0.0.1:52999/ws/agent")

    def test_remote_endpoints_cannot_redirect_or_leave_loopback(self):
        MODULE.loopback_url("http://127.0.0.1:8443/ready", ("http",))
        with self.assertRaisesRegex(ValueError, "loopback"):
            MODULE.loopback_url("http://10.0.0.2/ready", ("http",))
        with self.assertRaisesRegex(ValueError, "loopback"):
            MODULE.loopback_url("file:///etc/passwd", ("http",))


if __name__ == "__main__":
    unittest.main()
