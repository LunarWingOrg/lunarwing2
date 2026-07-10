"""Unit tests for lunarwing_mt_web."""

from __future__ import annotations

import unittest

from lunarwing_mt_web.bridge import SessionState, _log_activity
from lunarwing_mt_web.server import app


class TestSessionState(unittest.TestCase):
    def test_session_state_creation(self):
        s = SessionState(session_id="abc", tenant="test")
        self.assertEqual(s.status, "running")
        self.assertEqual(s.tenant, "test")

    def test_session_push_and_to_dict(self):
        s = SessionState(session_id="abc", tenant="test")
        s.push("hello")
        d = s.to_dict()
        self.assertEqual(d["tenant"], "test")
        self.assertEqual(d["status"], "running")

    def test_session_phase_tracking(self):
        s = SessionState(session_id="abc", tenant="test")
        s.phases.append({"name": "add-tenant", "ok": True, "returncode": 0})
        self.assertEqual(len(s.to_dict()["phases"]), 1)

    def test_session_error_field(self):
        s = SessionState(session_id="abc", tenant="test", error="something broke")
        self.assertEqual(s.to_dict()["error"], "something broke")

    def test_session_ok_status(self):
        s = SessionState(session_id="abc", tenant="test", status="ok")
        self.assertEqual(s.to_dict()["status"], "ok")

    def test_session_fail_status(self):
        s = SessionState(session_id="abc", tenant="test", status="fail")
        self.assertEqual(s.to_dict()["status"], "fail")


class TestFlaskRoutes(unittest.TestCase):
    def setUp(self):
        self.client = app.test_client()

    def test_index_returns_html(self):
        resp = self.client.get("/")
        self.assertEqual(resp.status_code, 200)

    def test_validate_name_empty_rejected(self):
        resp = self.client.post("/api/validate/name", json={"name": ""})
        data = resp.get_json()
        self.assertFalse(data["valid"])
        self.assertIsNotNone(data["error"])

    def test_validate_name_valid_accepted(self):
        resp = self.client.post("/api/validate/name", json={"name": "ruffles"})
        data = resp.get_json()
        self.assertTrue(data["valid"])

    def test_validate_name_reserved_rejected(self):
        resp = self.client.post("/api/validate/name", json={"name": "pg-test"})
        data = resp.get_json()
        self.assertFalse(data["valid"])

    def test_validate_name_uppercase_rejected(self):
        resp = self.client.post("/api/validate/name", json={"name": "Ruffles"})
        data = resp.get_json()
        self.assertFalse(data["valid"])

    def test_provision_missing_name_rejected(self):
        resp = self.client.post("/api/provision", json={"name": ""})
        self.assertEqual(resp.status_code, 400)

    def test_status_not_found(self):
        resp = self.client.get("/api/provision/nonexistent/status")
        self.assertEqual(resp.status_code, 404)

    def test_stream_not_found(self):
        resp = self.client.get("/api/provision/nonexistent/stream")
        self.assertEqual(resp.status_code, 404)


class TestActivityLog(unittest.TestCase):
    def test_log_activity_does_not_raise(self):
        _log_activity("test", "add-tenant", 0, 42.0)

    def test_log_activity_error_exit(self):
        _log_activity("test", "build-tenant", 1, 300.0)


if __name__ == "__main__":
    unittest.main()
