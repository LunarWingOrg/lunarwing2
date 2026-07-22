from __future__ import annotations

import os
import tempfile
import unittest
from unittest.mock import patch

from lunarwing_mt_onboard import provisioner, upgrade
from lunarwing_mt_onboard.upgrade_cli import UpgradeCliArgs, run_upgrade_flow
from lunarwing_mt_onboard.upgrade import UpgradeConfig
from lunarwing_mt_onboard.verify import CheckResult


class TestRunPreflight(unittest.TestCase):
    def test_run_preflight_returns_phase_result(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = os.path.join(tmp, "upgrade-preflight.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\necho hi\nexit 0\n")
            os.chmod(script, 0o700)

            previous = upgrade.PREFLIGHT_SCRIPT
            upgrade.PREFLIGHT_SCRIPT = script
            try:
                result = upgrade.run_preflight(UpgradeConfig(tenant="alpha"))
            finally:
                upgrade.PREFLIGHT_SCRIPT = previous

        self.assertTrue(result.ok)
        self.assertEqual(result.returncode, 0)
        self.assertIn("hi", result.stdout)


class TestRunUpgrade(unittest.TestCase):
    def test_upgrade_runs_preflight_then_upgrade(self):
        result = _run_upgrade_with_scripts(
            preflight_body="echo ok\nexit 0",
            upgrade_body="echo ok\nexit 0",
            cfg=UpgradeConfig(tenant="alpha"),
        )

        self.assertTrue(result.ok)
        self.assertEqual(len(result.phases), 2)

    def test_preflight_failure_stops_unless_forced(self):
        result = _run_upgrade_with_scripts(
            preflight_body="echo boom\nexit 1",
            upgrade_body="echo should-not-run\nexit 0",
            cfg=UpgradeConfig(tenant="alpha", force=False),
        )

        self.assertFalse(result.ok)
        self.assertEqual(len(result.phases), 1)

    def test_force_continues_past_failed_preflight(self):
        result = _run_upgrade_with_scripts(
            preflight_body="echo boom\nexit 1",
            upgrade_body="echo ran\nexit 0",
            cfg=UpgradeConfig(tenant="alpha", force=True),
        )

        self.assertEqual(len(result.phases), 2)
        self.assertFalse(result.phases[0].ok)
        self.assertTrue(result.phases[1].ok)


class TestUpgradeFlow(unittest.TestCase):
    def test_non_interactive_v2_upgrade_uses_mt_admin_and_verifies(self):
        with tempfile.TemporaryDirectory() as tmp:
            upgrade_script = _write_script(
                tmp,
                "lunarwing-mt-admin.sh",
                "echo \"$*\"\nexit 0",
            )

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = upgrade_script
            try:
                with patch(
                    "lunarwing_mt_onboard.upgrade_cli.verify_tenant",
                    return_value=[CheckResult("service lunarwing-alpha", True, "active")],
                ) as verify, patch(
                    "lunarwing_mt_onboard.upgrade_cli.tenant_gateway_port",
                    return_value=10020,
                ), patch(
                    "lunarwing_mt_onboard.upgrade_cli.tenant_gateway_host",
                    return_value="192.0.2.10",
                ):
                    code = run_upgrade_flow(
                        UpgradeCliArgs(
                            tenant="alpha",
                            target="v2.0.2.0",
                            source_repo="/srv/lunarwing",
                            no_backup=True,
                            skip_render=True,
                            apply=True,
                            yes=True,
                            non_interactive=True,
                            accept_defaults=False,
                            resume=None,
                            save=None,
                        )
                    )
                    verify.assert_called_once_with(
                        "alpha", host="192.0.2.10", port=10020
                    )
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        self.assertEqual(code, 0)

    def test_non_interactive_upgrade_requires_apply(self):
        code = run_upgrade_flow(
            UpgradeCliArgs(
                tenant="alpha",
                target="v2.0.2.0",
                non_interactive=True,
            )
        )
        self.assertEqual(code, 1)

    def test_upgrade_fails_verification_without_gateway_allocation(self):
        with tempfile.TemporaryDirectory() as tmp:
            upgrade_script = _write_script(
                tmp,
                "lunarwing-mt-admin.sh",
                "exit 0",
            )

            previous = provisioner.MT_ADMIN_SCRIPT
            provisioner.MT_ADMIN_SCRIPT = upgrade_script
            try:
                with patch(
                    "lunarwing_mt_onboard.upgrade_cli.verify_tenant",
                    return_value=[CheckResult("service lunarwing-alpha", True, "active")],
                ) as verify, patch(
                    "lunarwing_mt_onboard.upgrade_cli.tenant_gateway_port",
                    return_value=0,
                ), patch(
                    "lunarwing_mt_onboard.upgrade_cli.tenant_gateway_host",
                    return_value="127.0.0.1",
                ):
                    code = run_upgrade_flow(
                        UpgradeCliArgs(
                            tenant="alpha",
                            target="v2.0.2.0",
                            apply=True,
                            yes=True,
                            non_interactive=True,
                        )
                    )
                    verify.assert_called_once_with(
                        "alpha", host="127.0.0.1", port=0
                    )
            finally:
                provisioner.MT_ADMIN_SCRIPT = previous

        self.assertEqual(code, 2)


def _run_upgrade_with_scripts(
    *,
    preflight_body: str,
    upgrade_body: str,
    cfg: UpgradeConfig,
):
    with tempfile.TemporaryDirectory() as tmp:
        preflight = _write_script(tmp, "upgrade-preflight.sh", preflight_body)
        upgrade_script = _write_script(tmp, "upgrade-tenant-version.sh", upgrade_body)

        prev_pre = upgrade.PREFLIGHT_SCRIPT
        prev_up = upgrade.UPGRADE_SCRIPT
        upgrade.PREFLIGHT_SCRIPT = preflight
        upgrade.UPGRADE_SCRIPT = upgrade_script
        try:
            return upgrade.run_upgrade(cfg)
        finally:
            upgrade.PREFLIGHT_SCRIPT = prev_pre
            upgrade.UPGRADE_SCRIPT = prev_up


def _write_script(directory: str, name: str, body: str) -> str:
    path = os.path.join(directory, name)
    with open(path, "w") as f:
        f.write(f"#!/bin/sh\n{body}\n")
    os.chmod(path, 0o700)
    return path


if __name__ == "__main__":
    unittest.main()
