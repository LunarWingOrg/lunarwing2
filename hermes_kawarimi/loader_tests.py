"""Unit tests for loader wiring (flag construction, schema timeout).

No root or DB needed — these test the pure helper functions.
"""

from __future__ import annotations

import os
import unittest
from unittest import mock

from hermes_kawarimi import loader
from hermes_kawarimi.config import ImportPlan
from hermes_kawarimi.loader import _build_flags, _worker_flags, _schema_wait_seconds
from hermes_kawarimi.model import MappedAgent
from lunarwing_mt_onboard.provisioner import PhaseResult


class WorkerFlagsTest(unittest.TestCase):
    def test_empty_plan_yields_no_flags(self) -> None:
        self.assertEqual(_worker_flags(ImportPlan()), [])

    def test_nanocode_flag(self) -> None:
        plan = ImportPlan(with_nanocode=True)
        self.assertIn("--with-nanocode", _worker_flags(plan))

    def test_pebble_flag(self) -> None:
        plan = ImportPlan(with_pebble=True)
        self.assertIn("--with-pebble", _worker_flags(plan))

    def test_opencode_flag(self) -> None:
        plan = ImportPlan(with_opencode=True)
        self.assertIn("--with-opencode", _worker_flags(plan))

    def test_kimi_flag(self) -> None:
        plan = ImportPlan(with_kimi=True)
        self.assertIn("--with-kimi", _worker_flags(plan))


class BuildFlagsTest(unittest.TestCase):
    def test_empty_plan_yields_no_flags(self) -> None:
        self.assertEqual(_build_flags(ImportPlan()), [])

    def test_toolchains_flag(self) -> None:
        plan = ImportPlan(with_toolchains=True)
        self.assertEqual(_build_flags(plan), ["--with-toolchains"])

    def test_vision_not_in_build_flags(self) -> None:
        plan = ImportPlan(with_vision=True)
        self.assertEqual(_build_flags(plan), [])


class SchemaWaitSecondsTest(unittest.TestCase):
    def test_default_90(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("KAWARIMI_SCHEMA_WAIT_SECONDS", None)
            self.assertEqual(_schema_wait_seconds(), 90)

    def test_env_override(self) -> None:
        with mock.patch.dict(os.environ, {"KAWARIMI_SCHEMA_WAIT_SECONDS": "300"}):
            self.assertEqual(_schema_wait_seconds(), 300)

    def test_invalid_env_falls_back(self) -> None:
        with mock.patch.dict(os.environ, {"KAWARIMI_SCHEMA_WAIT_SECONDS": "abc"}):
            self.assertEqual(_schema_wait_seconds(), 90)

    def test_zero_or_negative_falls_back(self) -> None:
        with mock.patch.dict(os.environ, {"KAWARIMI_SCHEMA_WAIT_SECONDS": "0"}):
            self.assertEqual(_schema_wait_seconds(), 90)


class RunImportSequenceTest(unittest.TestCase):
    """The populate window must be daemon-down + PG-up.

    Regression test: mt-admin's stop-tenant also stops PostgreSQL, so the
    loader must issue start-pg after stop-tenant and before populate,
    otherwise the populate connection is refused.
    """

    def _run_phases(self, plan: ImportPlan) -> list[str]:
        verbs: list[str] = []

        def fake_run_command(args, **_kw):  # noqa: ANN001, ANN202
            verbs.append(args[1])
            return PhaseResult(name=args[1], returncode=0)

        env = {
            "DATABASE_BACKEND": "postgres",
            "DATABASE_URL": "postgres://example",
            "SECRETS_MASTER_KEY": "key",
            "LUNARWING_OWNER_ID": "owner",
        }
        with (
            mock.patch("hermes_kawarimi.loader.ensure_mt_admin", return_value="mt"),
            mock.patch(
                "hermes_kawarimi.loader.run_command", side_effect=fake_run_command
            ),
            mock.patch.object(loader.secrets_ops, "parse_tenant_env", return_value=env),
            mock.patch("hermes_kawarimi.loader._wait_for_schema"),
            mock.patch("hermes_kawarimi.loader._populate", return_value={}),
            mock.patch("hermes_kawarimi.loader._insert_secrets"),
        ):
            result = loader.run_import(plan, MappedAgent())
        self.assertTrue(result.ok, result.error)
        return verbs

    def test_start_pg_between_stop_tenant_and_populate(self) -> None:
        verbs = self._run_phases(ImportPlan(tenant="t1", apply=True))
        self.assertEqual(
            verbs,
            [
                "add-tenant",
                "build-tenant",
                "install-wasm",
                "start-tenant",
                "stop-tenant",
                "start-pg",
            ],
        )

    def test_start_after_populate_when_requested(self) -> None:
        verbs = self._run_phases(ImportPlan(tenant="t1", apply=True, start=True))
        self.assertEqual(verbs[-2:], ["start-pg", "start-tenant"])

    def test_secret_count_survives_in_result(self) -> None:
        """_insert_secrets writes counts["secrets"]; a later counts assignment
        must not clobber it (the CLI report depends on it)."""

        def fake_insert_secrets(_db, _mk, _owner, _mapped, result):  # noqa: ANN202
            result.counts["secrets"] = 3

        env = {
            "DATABASE_BACKEND": "postgres",
            "DATABASE_URL": "postgres://example",
            "SECRETS_MASTER_KEY": "key",
            "LUNARWING_OWNER_ID": "owner",
        }
        with (
            mock.patch("hermes_kawarimi.loader.ensure_mt_admin", return_value="mt"),
            mock.patch(
                "hermes_kawarimi.loader.run_command",
                return_value=PhaseResult(name="x", returncode=0),
            ),
            mock.patch.object(loader.secrets_ops, "parse_tenant_env", return_value=env),
            mock.patch("hermes_kawarimi.loader._wait_for_schema"),
            mock.patch(
                "hermes_kawarimi.loader._populate", return_value={"memory_docs": 1}
            ),
            mock.patch(
                "hermes_kawarimi.loader._insert_secrets",
                side_effect=fake_insert_secrets,
            ),
        ):
            result = loader.run_import(ImportPlan(tenant="t1", apply=True), MappedAgent())
        self.assertTrue(result.ok, result.error)
        self.assertEqual(result.counts, {"memory_docs": 1, "secrets": 3})


if __name__ == "__main__":
    unittest.main()
