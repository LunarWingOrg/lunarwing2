"""Unit + light integration tests for the web edition.

Run from the repo root with the venv interpreter:
    lunarwing_mt_onboard_web/.venv/bin/python -m unittest lunarwing_mt_onboard_web.web_tests
"""

from __future__ import annotations

import ast
import os
import queue
import signal
import tempfile
import time
import unittest
from pathlib import Path

from lunarwing_mt_onboard.config import WorkerType

from . import audit, demo
from .jobs import Job, JobManager
from .models import ExportRequest, ImportRequest, ProvisionRequest, UpgradeRequest
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

    def test_import_mapper_trims_strings_and_forces_noninteractive(self) -> None:
        cfg = ImportRequest(
            bundle="  /tmp/chimera.tar  ",
            name="  chimera-new  ",
            start=True,
            old_stopped=True,
            with_opencode=True,
            with_toolchains=True,
            with_nanocode=True,
            with_pebble=True,
            with_vision=True,
            docker_group=True,
            tensorzero_url="  http://tensorzero.test/openai/v1  ",
            owner_scope="  legacy-chimera  ",
            apply=True,
            force=True,
        ).to_import_config()

        self.assertEqual(cfg.bundle, "/tmp/chimera.tar")
        self.assertEqual(cfg.name, "chimera-new")
        self.assertEqual(cfg.tensorzero_url, "http://tensorzero.test/openai/v1")
        self.assertEqual(cfg.owner_scope, "legacy-chimera")
        self.assertTrue(cfg.start)
        self.assertTrue(cfg.old_stopped)
        self.assertTrue(cfg.with_opencode)
        self.assertTrue(cfg.with_toolchains)
        self.assertTrue(cfg.with_nanocode)
        self.assertTrue(cfg.with_pebble)
        self.assertTrue(cfg.with_vision)
        self.assertTrue(cfg.docker_group)
        self.assertTrue(cfg.apply)
        self.assertTrue(cfg.force)
        self.assertTrue(cfg.auto_yes)


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


class CancellationTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_cancel_terminates_wrapper_and_child_processes(self) -> None:
        from . import runner

        with tempfile.TemporaryDirectory() as tmp:
            script = Path(tmp) / "spawn-child.sh"
            child_pid_path = Path(tmp) / "child.pid"
            script.write_text(
                "#!/bin/sh\n"
                "trap '' TERM\n"
                "sleep 30 &\n"
                "child=$!\n"
                'printf \'%s\\n\' "$child" > "$1"\n'
                'wait "$child"\n'
            )
            script.chmod(0o700)

            manager = JobManager(demo=True, log_dir=tmp)
            job = manager.create("cancel-test")

            def run(job_to_run: Job) -> None:
                audit_log = audit.AuditLogger(
                    "cancel-test", "child", job_to_run.id, log_dir=tmp, demo=True
                )
                try:
                    runner._run_phase(
                        job_to_run,
                        [str(script), str(child_pid_path)],
                        "cancel-test",
                        audit_log,
                    )
                finally:
                    audit_log.close()

            manager.start(job, run)
            deadline = time.monotonic() + 3
            while (
                job.proc is None or not child_pid_path.exists()
            ) and time.monotonic() < deadline:
                time.sleep(0.01)

            self.assertIsNotNone(job.proc)
            self.assertTrue(child_pid_path.exists())
            parent_pid = job.proc.pid if job.proc is not None else None
            child_pid = int(child_pid_path.read_text().strip())
            try:
                self.assertTrue(manager.cancel(job.id))
                self.assertIsNotNone(job.thread)
                if job.thread is not None:
                    job.thread.join(timeout=5)
                    self.assertFalse(
                        job.thread.is_alive(), "cancel left a child process running"
                    )
            finally:
                for pid in (child_pid, parent_pid):
                    if pid is None:
                        continue
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if job.thread is not None:
                    job.thread.join(timeout=2)


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
        # credentials revealed during provision
        self.assertTrue(any(e.get("type") == "master_key" for e in events), events)
        gat = [e for e in events if e.get("type") == "gateway_auth_token"]
        self.assertTrue(gat, events)
        self.assertTrue(gat[0].get("token"), gat[0])
        self.assertEqual(gat[0].get("host"), "127.0.0.1")
        self.assertEqual(gat[0].get("port"), 10000)
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
            job, SecretRequest(tenant="sphinx", name="gotify_app_token", value=secret_value), log_dir=self.tmp
        )
        events = _drain(job.queue)
        self.assertTrue(any(e.get("type") == "secret_stored" for e in events))
        # the secret value must not appear in any audit log file
        for fn in os.listdir(self.tmp):
            with open(os.path.join(self.tmp, fn), encoding="utf-8") as f:
                self.assertNotIn(secret_value, f.read())

    def test_import_demo_is_noninteractive_dry_run_with_staged_output(self) -> None:
        job = Job(id="test4", mode="import", demo=True)
        req = ImportRequest(bundle="/tmp/chimera.tar")

        self.runner.run_import_job(job, req, log_dir=self.tmp)

        events = _drain(job.queue)
        phases = [e["name"] for e in events if e.get("type") == "phase"]
        log_text = "\n".join(e["line"] for e in events if e.get("type") == "log")
        done = [e for e in events if e.get("type") == "done"]
        self.assertEqual(phases, ["import"])
        for stage in ("Provision", "Build", "Inject", "Restore", "Stage"):
            self.assertIn(stage, log_text)
        self.assertTrue(done and done[-1]["ok"], f"expected success, got {done}")
        self.assertTrue(job.ok)

        audit_text = "\n".join(
            Path(self.tmp, name).read_text()
            for name in os.listdir(self.tmp)
            if "-import-" in name
        )
        self.assertIn("--dry-run", audit_text)
        self.assertIn("--yes", audit_text)
        self.assertNotIn("--start", audit_text)


class AppRouteTests(unittest.TestCase):
    def test_import_route_is_registered_as_post(self) -> None:
        app_path = Path(__file__).resolve().parent / "app.py"
        tree = ast.parse(app_path.read_text())
        post_paths = {
            decorator.args[0].value
            for node in ast.walk(tree)
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            for decorator in node.decorator_list
            if isinstance(decorator, ast.Call)
            and isinstance(decorator.func, ast.Attribute)
            and decorator.func.attr == "post"
            and decorator.args
            and isinstance(decorator.args[0], ast.Constant)
            and isinstance(decorator.args[0].value, str)
        }

        self.assertIn("/api/import", post_paths)


class ImportUiTests(unittest.TestCase):
    def test_import_card_and_form_are_wired(self) -> None:
        static_dir = Path(__file__).resolve().parent / "static"
        index = (static_dir / "index.html").read_text()
        wizard = (static_dir / "js" / "wizard.js").read_text()

        self.assertIn('data-mode="import"', index)
        self.assertIn("function mountImport", wizard)
        self.assertIn("mode === 'import'", wizard)

        import_form = wizard.split("function mountImport", 1)[1].split(
            "function mountSecrets", 1
        )[0]
        for field in (
            "bundle",
            "name",
            "apply",
            "start",
            "old_stopped",
            "force",
            "with_opencode",
            "with_toolchains",
            "with_nanocode",
            "with_pebble",
            "with_vision",
            "docker_group",
            "tensorzero_url",
            "owner_scope",
        ):
            self.assertIn(field, import_form)
        self.assertIn("stage-only by default", import_form.lower())
        self.assertIn("old host stopped", import_form.lower())


if __name__ == "__main__":
    unittest.main()


class SecretsUiTests(unittest.TestCase):
    def test_secret_name_placeholder_is_gotify_app_token(self) -> None:
        wizard = Path(__file__).resolve().parent / "static" / "js" / "wizard.js"
        text = wizard.read_text()
        self.assertIn("gotify_app_token", text)
        self.assertNotIn("openai_api_key", text)

