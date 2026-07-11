"""Unit tests for post-start verification (mt-admin status based)."""

from __future__ import annotations

import unittest
from types import SimpleNamespace

from lunarwing_mt_onboard.verify import (
    check_service_status,
    parse_daemon_state,
)


SYSTEMD_STATUS = """\
=== Tenant: lunarium ===

Ports:
  gateway:          10000
  http:             10001

PostgreSQL: running (lunarwing-pg-lunarium)
Nanocode worker: not created

Services (systemd):
  lunarwing-lunarium.service: active
  xmpp-bridge-lunarium.service: active
  lunarwing-proxy-lunarium.service: inactive
  lunarwing-weechat-lunarium.service: active
  lunarwing-weechat-adapter-lunarium.service: active
  lunarwing-pg-lunarium.service: active
  lunarwing-nanocode-lunarium.service: inactive
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


if __name__ == "__main__":
    unittest.main()
