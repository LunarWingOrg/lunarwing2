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

    def test_retired_nanocode_is_not_forwarded(self) -> None:
        plan = ImportPlan(tenant="legacy", with_nanocode=True)
        self.assertNotIn("--with-nanocode", _worker_flags(plan))
        self.assertIn("Nanocode worker is retired", plan.validate())

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
    """The populate window must be writer-quiesced + daemon-down + PG-up.

    Regression tests: mt-admin's stop-tenant also stops PostgreSQL, so the
    loader must issue start-pg after stop-tenant and before populate,
    otherwise the populate connection is refused. And since WI#133/#134,
    stop-writers must run before stop-tenant so the populate step can never
    race a writer container respawned by an OpenRC restart policy or
    babysitter.
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

    def _run_failure_path(
        self,
        plan: ImportPlan,
        *,
        populate_error: Exception | None = None,
        fail_on: tuple[str, int] | None = None,
        raise_on: tuple[str, int] | None = None,
    ) -> tuple[loader.ImportResult, list[str]]:
        verbs: list[str] = []
        occurrences: dict[str, int] = {}

        def fake_run_command(args, **_kw):  # noqa: ANN001, ANN202
            verb = args[1]
            verbs.append(verb)
            occurrences[verb] = occurrences.get(verb, 0) + 1
            current = (verb, occurrences[verb])
            if current == raise_on:
                raise RuntimeError(f"{verb} crashed")
            rc = 1 if current == fail_on else 0
            return PhaseResult(name=verb, returncode=rc)

        populate = mock.Mock(return_value={})
        if populate_error is not None:
            populate.side_effect = populate_error
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
            mock.patch("hermes_kawarimi.loader._populate", new=populate),
            mock.patch("hermes_kawarimi.loader._insert_secrets"),
        ):
            result = loader.run_import(plan, MappedAgent())
        return result, verbs

    def test_start_pg_between_stop_tenant_and_populate(self) -> None:
        verbs = self._run_phases(ImportPlan(tenant="t1", apply=True))
        self.assertEqual(
            verbs,
            [
                "add-tenant",
                "build-tenant",
                "install-wasm",
                "start-tenant",
                "stop-writers",
                "stop-tenant",
                "start-pg",
            ],
        )

    def test_stop_writers_failure_blocks_populate(self) -> None:
        """WI#133/#134: populate must never run unless writers are quiesced.

        stop-writers is the only mt-admin path that verifies quiescence
        (tenant_writers_active) and fails if any writer survived; a failed
        stop-writers must abort before stop-tenant/start-pg/populate.
        """
        result, verbs = self._run_failure_path(
            ImportPlan(tenant="t1", apply=True),
            fail_on=("stop-writers", 1),
        )

        self.assertEqual(
            result.error,
            "stop-writers failed (writers must be quiesced before populate)",
        )
        self.assertEqual(verbs, ["add-tenant", "build-tenant", "install-wasm", "start-tenant", "stop-writers"])
        self.assertNotIn("stop-tenant", verbs)
        self.assertNotIn("start-pg", verbs)

    def test_start_after_populate_when_requested(self) -> None:
        verbs = self._run_phases(ImportPlan(tenant="t1", apply=True, start=True))
        self.assertEqual(verbs[-2:], ["start-pg", "start-tenant"])

    def test_populate_failure_stops_owned_postgres(self) -> None:
        result, verbs = self._run_failure_path(
            ImportPlan(tenant="t1", apply=True),
            populate_error=RuntimeError("write failed"),
        )

        self.assertEqual(result.error, "populate failed: write failed")
        self.assertEqual(verbs[-2:], ["start-pg", "stop-pg"])

    def test_final_start_failure_stops_owned_postgres(self) -> None:
        result, verbs = self._run_failure_path(
            ImportPlan(tenant="t1", apply=True, start=True),
            fail_on=("start-tenant", 2),
        )

        self.assertEqual(result.error, "start-tenant failed (after populate)")
        self.assertEqual(verbs[-3:], ["start-pg", "start-tenant", "stop-pg"])

    def test_start_pg_failure_does_not_stop_unowned_postgres(self) -> None:
        result, verbs = self._run_failure_path(
            ImportPlan(tenant="t1", apply=True),
            fail_on=("start-pg", 1),
        )

        self.assertEqual(
            result.error,
            "start-pg failed (populate needs PostgreSQL up, daemon down)",
        )
        self.assertEqual(verbs[-2:], ["stop-tenant", "start-pg"])
        self.assertNotIn("stop-pg", verbs)

    def test_cleanup_failure_preserves_original_error(self) -> None:
        result, verbs = self._run_failure_path(
            ImportPlan(tenant="t1", apply=True),
            populate_error=RuntimeError("write failed"),
            fail_on=("stop-pg", 1),
        )

        self.assertEqual(result.error, "populate failed: write failed")
        self.assertEqual(verbs[-1], "stop-pg")
        self.assertTrue(any("cleanup failed" in warning for warning in result.warnings))

    def test_cleanup_exception_preserves_original_error(self) -> None:
        result, verbs = self._run_failure_path(
            ImportPlan(tenant="t1", apply=True),
            populate_error=RuntimeError("write failed"),
            raise_on=("stop-pg", 1),
        )

        self.assertEqual(result.error, "populate failed: write failed")
        self.assertEqual(verbs[-1], "stop-pg")
        self.assertTrue(any("cleanup raised" in warning for warning in result.warnings))

    def test_install_wasm_failure_fails_clearly(self) -> None:
        """install-wasm is required (import-tenant.sh aborts on it via set -e).

        Regression: the phase used to be 'best-effort' in name only — its
        failed PhaseResult still gated result.ok while result.error stayed
        None, so the CLI reported an unexplained failure after populate.
        """

        def fake_run_command(args, **_kw):  # noqa: ANN001, ANN202
            rc = 1 if args[1] == "install-wasm" else 0
            return PhaseResult(name=args[1], returncode=rc)

        with (
            mock.patch("hermes_kawarimi.loader.ensure_mt_admin", return_value="mt"),
            mock.patch(
                "hermes_kawarimi.loader.run_command", side_effect=fake_run_command
            ),
        ):
            result = loader.run_import(ImportPlan(tenant="t1", apply=True), MappedAgent())
        self.assertFalse(result.ok)
        self.assertEqual(result.error, "install-wasm failed")

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
