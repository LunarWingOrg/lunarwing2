"""Unit tests for post-start verification (mt-admin status based)."""

from __future__ import annotations

import unittest
from types import SimpleNamespace
from unittest.mock import patch

from lunarwing_mt_onboard.verify import (
    CheckResult,
    check_service_status,
    check_tenant_status,
    parse_daemon_state,
    verify_tenant,
)


SYSTEMD_STATUS = """\
=== Tenant: lunarium ===

Ports:
  gateway:          10000
  http:             10001

PostgreSQL: running (lunarwing-pg-lunarium)
Nanocode (retired): no container in active runtime store

Services (systemd):
  lunarwing-lunarium.service: active
  xmpp-bridge-lunarium.service: active
  lunarwing-proxy-lunarium.service: inactive
  lunarwing-weechat-lunarium.service: active
  lunarwing-weechat-adapter-lunarium.service: active
  lunarwing-pg-lunarium.service: active
"""

OPENRC_STATUS = """\
=== Tenant: gentoo-t ===

Services (openrc):
  lunarwing-gentoo-t: started
  xmpp-bridge-gentoo-t: started
  lunarwing-pg-gentoo-t: started
"""

STOPPED_STATUS = """\
=== Tenant: lunarium ===

Services (systemd):
  lunarwing-lunarium.service: inactive
  xmpp-bridge-lunarium.service: inactive
"""


class ParseDaemonStateTests(unittest.TestCase):
    def test_systemd_active(self) -> None:
        ok, detail = parse_daemon_state(SYSTEMD_STATUS, "lunarium")
        self.assertTrue(ok)
        self.assertEqual(detail, "active")

    def test_openrc_started(self) -> None:
        ok, detail = parse_daemon_state(OPENRC_STATUS, "gentoo-t")
        self.assertTrue(ok)
        self.assertEqual(detail, "started")

    def test_inactive(self) -> None:
        ok, detail = parse_daemon_state(STOPPED_STATUS, "lunarium")
        self.assertFalse(ok)
        self.assertEqual(detail, "inactive")

    def test_missing_line(self) -> None:
        ok, detail = parse_daemon_state("=== Tenant: x ===\nno services\n", "x")
        self.assertIsNone(ok)
        self.assertIn("not found", detail)

    def test_does_not_match_worker_prefix(self) -> None:
        # Only the primary daemon line, not lunarwing-nanocode-<t>
        text = "  lunarwing-nanocode-lunarium.service: active\n"
        ok, _ = parse_daemon_state(text, "lunarium")
        self.assertIsNone(ok)


class CheckServiceStatusTests(unittest.TestCase):
    def test_uses_mt_admin_status_argv(self) -> None:
        seen: list[list[str]] = []

        def runner(argv: list[str]):
            seen.append(list(argv))
            return SimpleNamespace(returncode=0, stdout=SYSTEMD_STATUS, stderr="")

        result = check_service_status(
            "lunarium", script="/fake/mt-admin.sh", runner=runner
        )
        self.assertEqual(seen, [["/fake/mt-admin.sh", "status", "lunarium"]])
        self.assertTrue(result.ok)
        self.assertEqual(result.detail, "active")
        self.assertEqual(result.label, "service lunarwing-lunarium")

    def test_nonzero_exit_is_fail(self) -> None:
        def runner(argv: list[str]):
            return SimpleNamespace(
                returncode=1,
                stdout="tenant 'nope' not found in registry\n",
                stderr="",
            )

        result = check_service_status("nope", script="/fake/mt-admin.sh", runner=runner)
        self.assertFalse(result.ok)
        self.assertIn("not found", result.detail)

    def test_openrc_via_runner(self) -> None:
        def runner(argv: list[str]):
            return SimpleNamespace(returncode=0, stdout=OPENRC_STATUS, stderr="")

        result = check_service_status(
            "gentoo-t", script="/fake/mt-admin.sh", runner=runner
        )
        self.assertTrue(result.ok)
        self.assertEqual(result.detail, "started")


class RuntimeVerificationTests(unittest.TestCase):
    def checks(self, text: str, tenant: str = "lunarium", workers=("pebble", "opencode")):
        def runner(argv):
            self.assertEqual(argv, ["/fake/mt-admin.sh", "status", tenant])
            return SimpleNamespace(returncode=0, stdout=text, stderr="")

        return check_tenant_status(tenant, workers=workers, script="/fake/mt-admin.sh", runner=runner)

    def test_active_units_do_not_hide_broken_container_runtime(self):
        text = SYSTEMD_STATUS.replace("PostgreSQL: running", "PostgreSQL: stopped")
        text += "Pebble worker: not created\nOpenCode worker: not created\n"
        text += "  lunarwing-pebble-lunarium.service: active\n"
        checks = self.checks(text)
        self.assertTrue(checks[0].ok)
        self.assertEqual([c.label for c in checks if not c.ok], ["PostgreSQL", "Pebble worker", "OpenCode worker"])

    def test_both_init_shapes_require_running_selected_containers(self):
        for tenant, text in (("lunarium", SYSTEMD_STATUS), ("gentoo-t", OPENRC_STATUS)):
            with self.subTest(tenant=tenant):
                if "PostgreSQL:" not in text:
                    text += "PostgreSQL: running (lunarwing-pg-gentoo-t)\n"
                text += "Pebble worker: running (fixture)\nOpenCode worker: running (fixture)\n"
                text += "Kimi worker: not created\nOpenCode 2 worker: not created\n"
                self.assertTrue(all(c.ok for c in self.checks(text, tenant)))

    def test_missing_selected_worker_line_fails_closed(self):
        checks = self.checks(SYSTEMD_STATUS, workers=("pebble",))
        self.assertFalse(checks[-1].ok)
        self.assertIn("not found", checks[-1].detail)

    def test_stopped_selected_worker_is_rejected(self):
        checks = self.checks(SYSTEMD_STATUS + "Pebble worker: stopped (fixture)\n", workers=("pebble",))
        self.assertFalse(checks[-1].ok)

    def test_unselected_workers_are_not_required(self):
        self.assertTrue(all(c.ok for c in self.checks(SYSTEMD_STATUS, workers=())))

    def test_poll_does_not_finish_at_daemon_only_success(self):
        failed = self.checks(SYSTEMD_STATUS + "Pebble worker: not created\n", workers=("pebble",))
        healthy = self.checks(SYSTEMD_STATUS + "Pebble worker: running (fixture)\n", workers=("pebble",))
        with patch("lunarwing_mt_onboard.verify.check_tenant_status", side_effect=[failed, healthy]) as probe, \
             patch("lunarwing_mt_onboard.verify.time.sleep"):
            result = verify_tenant("lunarium", workers=["pebble"])
        self.assertEqual(result, healthy)
        self.assertEqual(probe.call_count, 2)
        probe.assert_called_with("lunarium", workers=["pebble"], script=None)


class CliVerificationTests(unittest.TestCase):
    def test_missing_selected_worker_fails_cli_exit(self):
        from lunarwing_mt_onboard import cli
        from lunarwing_mt_onboard.config import TenantConfig, WorkerType

        cfg = TenantConfig(name="lunarium", workers=[WorkerType.PEBBLE], secrets_master_key="fixture")
        with patch.object(cli, "gather_config", return_value=cfg), \
             patch.object(cli, "ensure_mt_admin"), \
             patch.object(cli, "_show_summary", return_value=True), \
             patch.object(cli, "provision", return_value=SimpleNamespace(ok=True)), \
             patch.object(cli, "_display_results"), \
             patch.object(cli, "console"), \
             patch.object(cli, "verify_tenant", return_value=[CheckResult("Pebble worker", False, "not created")]) as verify:
            self.assertEqual(cli.main([]), 2)
        verify.assert_called_once_with("lunarium", "127.0.0.1", 0, workers=["pebble"])


if __name__ == "__main__":
    unittest.main()
