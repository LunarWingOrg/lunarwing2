"""Unit + light integration tests for the web edition.

Run from the repo root with the venv interpreter:
    lunarwing_mt_onboard_web/.venv/bin/python -m unittest lunarwing_mt_onboard_web.web_tests
"""

from __future__ import annotations

import os
import queue
import tempfile
import unittest

from lunarwing_mt_onboard.config import WorkerType

from . import audit, demo
from .jobs import Job
from .models import ExportRequest, ProvisionRequest, UpgradeRequest
from .security import generate_token, token_matches


class ModelMappingTests(unittest.TestCase):
    def test_provision_maps_fields_and_workers(self) -> None:
        req = ProvisionRequest(
            name="  sphinx  ",
            workers=["nanocode", "bogus", "opencode"],
            no_ssh=True,
            xmpp_allow_from=["a@x", "  ", "b@y"],
        )
        cfg = req.to_tenant_config()
        self.assertEqual(cfg.name, "sphinx")
        self.assertEqual(cfg.workers, [WorkerType.NANOCODE, WorkerType.OPENCODE])
        self.assertTrue(cfg.no_ssh)
        self.assertEqual(cfg.xmpp_allow_from, ["a@x", "b@y"])

    def test_upgrade_and_export_mappers(self) -> None:
        u = UpgradeRequest(tenant="griffin", target="v1.1.9", apply=True).to_upgrade_config()
        self.assertEqual(u.tenant, "griffin")
        self.assertEqual(u.target, "v1.1.9")
        self.assertTrue(u.apply)
        e = ExportRequest(tenant="chimera").to_export_config()
        self.assertEqual(e.tenant, "chimera")
        self.assertEqual(e.out_dir, "/var/lib/lunarwing-migrate")


class RedactionTests(unittest.TestCase):
    def test_redact_kv_and_hex(self) -> None:
        key = "a" * 64
        red = audit.redact_text(f"SECRETS_MASTER_KEY='{key}'")
        self.assertNotIn(key, red)
        self.assertIn("REDACTED", red)
        self.assertNotIn("b" * 64, audit.redact_text("token=" + "b" * 64))

    def test_redact_argv_after_secret_flag(self) -> None:
        argv = ["mt", "add-tenant", "x", "--xmpp-password", "hunter2", "--gateway-host", "127.0.0.1"]
        red = audit.redact_argv(argv)
        self.assertNotIn("hunter2", red)
        self.assertIn("127.0.0.1", red)  # non-secret flags untouched

    def test_plain_line_unchanged(self) -> None:
        self.assertEqual(audit.redact_text("Compiling lunarwing v1.1.9"), "Compiling lunarwing v1.1.9")


class TokenTests(unittest.TestCase):
    def test_token_match(self) -> None:
        t = generate_token()
        self.assertTrue(token_matches(t, t))
        self.assertFalse(token_matches(t, "nope"))
        self.assertFalse(token_matches(t, None))
        self.assertTrue(token_matches("", "anything"))  # empty = disabled


def _drain(q: "queue.Queue") -> list[dict]:
    out = []
    while True:
        try:
            out.append(q.get_nowait())
        except queue.Empty:
            break
    return out


class DemoProvisionIntegrationTests(unittest.TestCase):
    def setUp(self) -> None:
        os.environ["LUNARWING_DEMO_SLEEP"] = "0"
        demo.enable()
        # import after demo.enable so patched globals are in effect
        from . import runner

        self.runner = runner
        self.tmp = tempfile.mkdtemp(prefix="lw-web-test-")

    def test_provision_demo_emits_phases_and_succeeds(self) -> None:
        job = Job(id="test1", mode="provision", demo=True)
        req = ProvisionRequest(name="sphinx", workers=["nanocode"], skip_start=True)
        self.runner.run_provision_job(job, req, log_dir=self.tmp)
        events = _drain(job.queue)
        phases = [e["name"] for e in events if e.get("type") == "phase"]
        self.assertIn("add-tenant", phases)
        self.assertIn("build-tenant", phases)
        done = [e for e in events if e.get("type") == "done"]
        self.assertTrue(done and done[-1]["ok"], f"expected success, got {done}")
        self.assertTrue(job.ok)

    def test_provision_demo_failure_path(self) -> None:
        job = Job(id="test2", mode="provision", demo=True)
        req = ProvisionRequest(name="failme", skip_start=True)  # 'fail' triggers build failure
        self.runner.run_provision_job(job, req, log_dir=self.tmp)
        events = _drain(job.queue)
        done = [e for e in events if e.get("type") == "done"]
        self.assertTrue(done and not done[-1]["ok"], "expected failure for 'failme'")

    def test_secret_demo_does_not_log_value(self) -> None:
        from .models import SecretRequest

        job = Job(id="test3", mode="secrets", demo=True)
        secret_value = "sup3r-s3cret-v4lue-xyz"
        self.runner.run_secret_job(
            job, SecretRequest(tenant="sphinx", name="openai_api_key", value=secret_value), log_dir=self.tmp
        )
        events = _drain(job.queue)
        self.assertTrue(any(e.get("type") == "secret_stored" for e in events))
        # the secret value must not appear in any audit log file
        for fn in os.listdir(self.tmp):
            with open(os.path.join(self.tmp, fn), encoding="utf-8") as f:
                self.assertNotIn(secret_value, f.read())


if __name__ == "__main__":
    unittest.main()
