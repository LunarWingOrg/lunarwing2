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
from unittest.mock import patch

from pydantic import ValidationError

from lunarwing_mt_onboard.config import WorkerType

from . import audit, demo
from .__main__ import _is_loopback_host
from .jobs import Job, JobConflictError, JobManager
from .models import ExportRequest, ImportRequest, ProvisionRequest, UpgradeRequest
from .security import generate_token, token_matches

try:
    from fastapi.testclient import TestClient

    from .app import create_app
except ModuleNotFoundError:
    TestClient = None
    create_app = None


class ModelMappingTests(unittest.TestCase):
    def test_request_models_reject_unknown_fields(self) -> None:
        with self.assertRaises(ValidationError):
            UpgradeRequest(
                tenant="alpha",
                target="v2.0.2.0",
                apply=True,
                source_reop="/srv/lunarwing",
            )

    def test_provision_rejects_unknown_workers(self) -> None:
        with self.assertRaises(ValidationError):
            ProvisionRequest(name="alpha", workers=["opencdoe"])

    def test_obsolete_tensorzero_url_is_rejected(self) -> None:
        with self.assertRaises(ValidationError):
            ProvisionRequest(
                name="alpha",
                tensorzero_url="http://proxy.example.test/openai/v1",
            )

    def test_provision_maps_fields_and_workers(self) -> None:
        req = ProvisionRequest(
            name="  sphinx  ",
            workers=["nanocode", "opencode"],
            no_ssh=True,
            no_weechat_bootstrap=True,
            xmpp_allow_from=["a@x", "  ", "b@y"],
        )
        cfg = req.to_tenant_config()
        self.assertEqual(cfg.name, "sphinx")
        self.assertEqual(cfg.workers, [WorkerType.NANOCODE, WorkerType.OPENCODE])
        self.assertTrue(cfg.no_ssh)
        self.assertTrue(cfg.no_weechat_bootstrap)
        self.assertEqual(cfg.xmpp_allow_from, ["a@x", "b@y"])

    def test_provision_weechat_bootstrap_defaults_false(self) -> None:
        """When omitted, no_weechat_bootstrap defaults to False."""
        req = ProvisionRequest(name="sphinx")
        cfg = req.to_tenant_config()
        self.assertFalse(cfg.no_weechat_bootstrap)

    def test_upgrade_and_export_mappers(self) -> None:
        u = UpgradeRequest(
            tenant="griffin",
            target="v2.0.2.0",
            source_repo=" /srv/lunarwing ",
            no_backup=True,
            skip_render=True,
            apply=True,
        ).to_upgrade_config()
        self.assertEqual(u.tenant, "griffin")
        self.assertEqual(u.target, "v2.0.2.0")
        self.assertEqual(u.source_repo, "/srv/lunarwing")
        self.assertTrue(u.no_backup)
        self.assertTrue(u.skip_render)
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
            owner_scope="  legacy-chimera  ",
            apply=True,
            force=True,
        ).to_import_config()

        self.assertEqual(cfg.bundle, "/tmp/chimera.tar")
        self.assertEqual(cfg.name, "chimera-new")
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

    def test_current_provision_fields_map_to_tenant_config(self) -> None:
        cfg = ProvisionRequest(
            name="sphinx",
            workers=["nanocode", "opencode"],
            llm_base_url=" http://llm.test/v1 ",
            nanocode_model=" nano ",
            nanocode_base_url=" http://nano.test/v1 ",
            opencode_model=" open ",
            opencode_base_url=" http://open.test/v1 ",
        ).to_tenant_config()

        self.assertEqual(cfg.llm_base_url, "http://llm.test/v1")
        self.assertEqual(cfg.nanocode_model, "nano")
        self.assertEqual(cfg.nanocode_base_url, "http://nano.test/v1")
        self.assertEqual(cfg.opencode_model, "open")
        self.assertEqual(cfg.opencode_base_url, "http://open.test/v1")

    def test_worker_overrides_are_dropped_when_worker_is_not_selected(self) -> None:
        cfg = ProvisionRequest(
            name="sphinx",
            nanocode_model="stale-nano",
            opencode_model="stale-open",
        ).to_tenant_config()
        self.assertEqual(cfg.nanocode_model, "")
        self.assertEqual(cfg.opencode_model, "")

    def test_invalid_master_key_and_skip_build_start_mismatch_are_rejected(self) -> None:
        with self.assertRaises(ValidationError):
            ProvisionRequest(name="sphinx", secrets_master_key="not-hex")
        with self.assertRaises(ValidationError):
            ProvisionRequest(name="sphinx", skip_build=True, skip_start=False)

    def test_export_passphrase_is_confirmed_and_excluded_from_dump(self) -> None:
        secret = "correct horse battery staple"
        request = ExportRequest(
            tenant="sphinx",
            apply=True,
            passphrase=secret,
            passphrase_confirm=secret,
        )

        self.assertEqual(request.to_export_config().passphrase, secret)
        self.assertNotIn("passphrase", request.model_dump())
        self.assertNotIn("passphrase_confirm", request.model_dump())
        self.assertNotIn(secret, repr(request))
        with self.assertRaises(ValidationError):
            ExportRequest(
                tenant="sphinx",
                apply=True,
                passphrase=secret,
                passphrase_confirm="different password",
            )

    def test_encrypted_import_requires_passphrase_but_legacy_tar_does_not(self) -> None:
        with self.assertRaises(ValidationError):
            ImportRequest(bundle="/tmp/sphinx.7z")
        request = ImportRequest(bundle="/tmp/sphinx.7z", passphrase="legacy")
        self.assertEqual(request.to_import_config().passphrase, "legacy")
        self.assertNotIn("passphrase", request.model_dump())
        ImportRequest(bundle="/tmp/sphinx.tar")

    def test_import_start_requires_old_host_stopped_confirmation(self) -> None:
        with self.assertRaises(ValidationError):
            ImportRequest(bundle="/tmp/sphinx.tar", start=True, old_stopped=False)
        ImportRequest(bundle="/tmp/sphinx.tar", start=True, old_stopped=True)

    def test_upgrade_requires_target_and_explicit_confirmation(self) -> None:
        with self.assertRaises(ValidationError):
            UpgradeRequest(tenant="sphinx", target="v2.0.2.0")
        with self.assertRaises(ValidationError):
            UpgradeRequest(tenant="sphinx", target="", apply=True)


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

    def test_audit_file_is_always_owner_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            logger = audit.AuditLogger("export", "sphinx", "mode", log_dir=tmp)
            path = logger.path
            logger.close()
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)


class TokenTests(unittest.TestCase):
    def test_token_match(self) -> None:
        t = generate_token()
        self.assertTrue(token_matches(t, t))
        self.assertFalse(token_matches(t, "nope"))
        self.assertFalse(token_matches(t, None))
        self.assertTrue(token_matches("", "anything"))  # empty = disabled


class BindSecurityTests(unittest.TestCase):
    def test_full_loopback_ranges_are_recognized(self) -> None:
        for host in ("localhost", "127.0.0.1", "127.0.0.2", "::1", "[::1]"):
            with self.subTest(host=host):
                self.assertTrue(_is_loopback_host(host))
        self.assertFalse(_is_loopback_host("0.0.0.0"))


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


class JobExclusionTests(unittest.TestCase):
    def test_second_privileged_job_is_rejected_until_first_finishes(self) -> None:
        manager = JobManager(demo=True)
        first = manager.create("export")
        with self.assertRaises(JobConflictError):
            manager.create("import")

        first.status = "done"
        second = manager.create("import")
        self.assertEqual(second.mode, "import")


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

    def test_encrypted_export_passphrase_never_reaches_events_or_audit(self) -> None:
        secret = "correct horse battery staple"
        job = Job(id="test5", mode="export", demo=True)
        req = ExportRequest(
            tenant="sphinx",
            apply=True,
            passphrase=secret,
            passphrase_confirm=secret,
        )

        self.runner.run_export_job(job, req, log_dir=self.tmp)

        events = _drain(job.queue)
        self.assertTrue(job.ok, events)
        self.assertNotIn(secret, repr(events))
        for name in os.listdir(self.tmp):
            self.assertNotIn(secret, Path(self.tmp, name).read_text())

    def test_encrypted_import_passphrase_never_reaches_events_or_audit(self) -> None:
        secret = "bundle passphrase"
        job = Job(id="test6", mode="import", demo=True)
        req = ImportRequest(bundle="/tmp/chimera.7z", passphrase=secret)

        self.runner.run_import_job(job, req, log_dir=self.tmp)

        events = _drain(job.queue)
        self.assertTrue(job.ok, events)
        self.assertNotIn(secret, repr(events))
        for name in os.listdir(self.tmp):
            self.assertNotIn(secret, Path(self.tmp, name).read_text())

    def test_verification_failure_fails_overall_job(self) -> None:
        job = Job(id="test7", mode="provision", demo=True)
        req = ProvisionRequest(name="sphinx")
        failed_check = [{"label": "gateway port", "ok": False, "detail": "closed"}]

        with patch.object(self.runner, "_verify", return_value=failed_check):
            self.runner.run_provision_job(job, req, log_dir=self.tmp)

        events = _drain(job.queue)
        done = [event for event in events if event.get("type") == "done"]
        self.assertTrue(done)
        self.assertFalse(done[-1]["ok"])
        self.assertIn(
            {"name": "verify", "ok": False, "code": 1},
            done[-1]["phases"],
        )

    def test_v2_upgrade_uses_mt_admin_lifecycle(self) -> None:
        job = Job(id="test8", mode="upgrade", demo=True)
        req = UpgradeRequest(
            tenant="sphinx",
            target="v2.0.2.0",
            source_repo="/srv/lunarwing",
            no_backup=True,
            skip_render=True,
            apply=True,
        )

        self.runner.run_upgrade_job(job, req, log_dir=self.tmp)

        events = _drain(job.queue)
        self.assertTrue(job.ok, events)
        audit_text = "\n".join(
            Path(self.tmp, name).read_text()
            for name in os.listdir(self.tmp)
            if "-upgrade-" in name
        )
        self.assertIn("upgrade-tenant sphinx --target v2.0.2.0", audit_text)
        self.assertIn("--source-repo /srv/lunarwing", audit_text)
        self.assertIn("--no-backup", audit_text)
        self.assertIn("--skip-render", audit_text)


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

    @unittest.skipIf(TestClient is None, "FastAPI test dependencies are not installed")
    def test_validation_response_never_echoes_passphrase(self) -> None:
        secret = "correct horse battery staple"
        with tempfile.TemporaryDirectory() as tmp:
            assert TestClient is not None and create_app is not None
            client = TestClient(create_app(token="test-token", demo=True, log_dir=tmp))
            response = client.post(
                "/api/export?token=test-token",
                json={
                    "tenant": "sphinx",
                    "apply": True,
                    "passphrase": secret,
                    "passphrase_confirm": "different passphrase",
                },
            )

        self.assertEqual(response.status_code, 422)
        self.assertNotIn(secret, response.text)
        self.assertNotIn("different passphrase", response.text)
        self.assertNotIn("input", response.json()["detail"][0])


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
            "owner_scope",
            "passphrase",
        ):
            self.assertIn(field, import_form)
        self.assertIn("stage-only by default", import_form.lower())
        self.assertIn("old host stopped", import_form.lower())

    def test_export_and_upgrade_forms_match_current_contracts(self) -> None:
        wizard = (Path(__file__).resolve().parent / "static" / "js" / "wizard.js").read_text()
        export_form = wizard.split("function mountExport", 1)[1].split(
            "function mountImport", 1
        )[0]
        upgrade_form = wizard.split("function mountUpgrade", 1)[1].split(
            "function mountExport", 1
        )[0]

        self.assertIn("passphrase_confirm", export_form)
        self.assertIn("at least 12 characters", export_form)
        self.assertIn("source_repo", upgrade_form)
        self.assertIn("no_backup", upgrade_form)
        self.assertIn("skip_render", upgrade_form)
        self.assertNotIn("upgrade-tenant-version.sh", upgrade_form)


class ProvisionWeechatUiTests(unittest.TestCase):
    def test_wizard_js_contains_weechat_bootstrap_control(self) -> None:
        wizard = (Path(__file__).resolve().parent / "static" / "js" / "wizard.js").read_text()
        self.assertIn("weechat_bootstrap: true", wizard)
        self.assertIn("no_weechat_bootstrap: !data.weechat_bootstrap", wizard)
        self.assertIn("Automatically configure WeeChat relay", wizard)


if __name__ == "__main__":
    unittest.main()


class SecretsUiTests(unittest.TestCase):
    def test_secret_name_placeholder_is_gotify_app_token(self) -> None:
        wizard = Path(__file__).resolve().parent / "static" / "js" / "wizard.js"
        text = wizard.read_text()
        self.assertIn("gotify_app_token", text)
        self.assertNotIn("openai_api_key", text)


class MascotSpriteTests(unittest.TestCase):
    def test_all_mascot_gifs_present(self) -> None:
        mascot = Path(__file__).resolve().parent / "static" / "mascot"
        for name in (
            "lunar_walk.gif",
            "lunar_jump.gif",
            "lunar_sleep.gif",
            "lunar_rage.gif",
            "lunar_greet.gif",
            "lunar_sup.gif",
            "lunar_fiery.gif",
            "lunar_love.gif",
        ):
            self.assertTrue((mascot / name).is_file(), f"missing mascot sprite: {name}")

    def test_bat_js_wires_greet_and_sup_moods(self) -> None:
        bat = Path(__file__).resolve().parent / "static" / "js" / "bat.js"
        text = bat.read_text()
        # both new moods mapped to files
        self.assertIn("greet: 'lunar_greet.gif'", text)
        self.assertIn("sup: 'lunar_sup.gif'", text)
        # one-shot API exported and event moods kept out of the idle rotation
        self.assertIn("greet,", text)
        self.assertIn("celebrate,", text)
        # greet/sup stay out of the idle rotation; fiery + love are idle moods
        self.assertIn("IDLE_MOODS = ['content', 'excited', 'sleeping', 'fiery', 'love']", text)
        self.assertIn("fiery: 'lunar_fiery.gif'", text)
        self.assertIn("love: 'lunar_love.gif'", text)

    def test_app_js_triggers_greet_and_celebrate(self) -> None:
        app = Path(__file__).resolve().parent / "static" / "js" / "app.js"
        text = app.read_text()
        self.assertIn("bat.greet()", text)
        self.assertIn("bat.celebrate()", text)
