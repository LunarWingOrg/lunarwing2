#!/usr/bin/env python3

import importlib.util
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("health_server", Path(__file__).with_name("health_server.py"))
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def stamp(delta=timedelta()):
    return (datetime.now(timezone.utc) + delta).isoformat().replace("+00:00", "Z")


class HealthTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        MODULE.WS_STATE_FILE = str(root / "ws.json")
        MODULE.RUNNER_STATE_FILE = str(root / "runner.json")
        MODULE.GATE_STATE_FILE = str(root / "gate.json")
        self.write(MODULE.WS_STATE_FILE, {"ready": True, "listening": True})
        self.write(MODULE.RUNNER_STATE_FILE, {"ready": True, "heartbeat_at": stamp()})
        self.write(MODULE.GATE_STATE_FILE, {
            "ready": True,
            "composition": True,
            "sandbox": True,
            "task": True,
            "heartbeat_at": stamp(),
        })

    def tearDown(self):
        self.directory.cleanup()

    @staticmethod
    def write(path, value):
        Path(path).write_text(json.dumps(value), encoding="utf-8")

    def test_ready_requires_all_three_live_components(self):
        body, status = MODULE.readiness()
        self.assertEqual(status, 200)
        self.assertTrue(body["ready"])
        self.assertEqual(body["task_gate"], {"ready": True})

    def test_stale_or_partial_gate_is_not_ready(self):
        self.write(MODULE.GATE_STATE_FILE, {"ready": True, "composition": True, "sandbox": True, "task": True, "heartbeat_at": stamp(timedelta(seconds=-45))})
        body, status = MODULE.readiness()
        self.assertEqual(status, 503)
        self.assertFalse(body["ready"])
        self.write(MODULE.GATE_STATE_FILE, {"ready": True, "composition": True, "sandbox": False, "task": True, "heartbeat_at": stamp()})
        body, status = MODULE.readiness()
        self.assertEqual(status, 503)
        self.assertFalse(body["task_gate"]["ready"])

    def test_future_and_malformed_heartbeats_fail_closed(self):
        self.write(MODULE.RUNNER_STATE_FILE, {"ready": True, "heartbeat_at": stamp(timedelta(seconds=30))})
        _, status = MODULE.readiness()
        self.assertEqual(status, 503)
        Path(MODULE.RUNNER_STATE_FILE).write_text("not-json", encoding="utf-8")
        _, status = MODULE.readiness()
        self.assertEqual(status, 503)

    def test_health_contains_no_credentials_or_model_route(self):
        body = MODULE.health()
        self.assertNotIn("LUNARDEPTH_API_KEY", json.dumps(body))
        self.assertNotIn("base_url", body)
        self.assertNotIn("model", body)


if __name__ == "__main__":
    unittest.main()
