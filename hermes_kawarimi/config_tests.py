"""Unit tests for ImportPlan serialization and validation. No root/DB."""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from unittest.mock import patch

from hermes_kawarimi.config import ImportPlan
from hermes_kawarimi.cli import _plan_from_args, build_parser, main


class ImportPlanSerializationTest(unittest.TestCase):
    def test_roundtrip_preserves_all_fields(self) -> None:
        plan = ImportPlan(
            source="/tmp/hermes",
            tenant="testimport",
            apply=True,
            start=True,
            force=True,
            with_pebble=True,
            with_opencode=True,
            with_kimi=True,
            with_toolchains=True,
            with_vision=False,
            tensorzero_url="http://tz:3000",
            llm_model="claude-sonnet",
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "plan.json")
            plan.to_json(path)
            loaded = ImportPlan.from_json(path)
        self.assertEqual(loaded.source, "/tmp/hermes")
        self.assertEqual(loaded.tenant, "testimport")
        self.assertTrue(loaded.apply)
        self.assertTrue(loaded.start)
        self.assertTrue(loaded.force)
        self.assertTrue(loaded.with_pebble)
        self.assertTrue(loaded.with_opencode)
        self.assertTrue(loaded.with_kimi)
        self.assertTrue(loaded.with_toolchains)
        self.assertFalse(loaded.with_vision)
        self.assertEqual(loaded.tensorzero_url, "http://tz:3000")
        self.assertEqual(loaded.llm_model, "claude-sonnet")

    def test_no_auto_yes_field(self) -> None:
        plan = ImportPlan()
        data = plan.to_dict()
        self.assertNotIn("auto_yes", data)

    def test_from_dict_ignores_unknown_keys(self) -> None:
        plan = ImportPlan.from_dict(
            {"tenant": "x", "auto_yes": True, "unknown": 42}
        )
        self.assertEqual(plan.tenant, "x")

    def test_json_file_permissions(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "plan.json")
            ImportPlan(tenant="x").to_json(path)
            mode = os.stat(path).st_mode & 0o777
            self.assertEqual(mode, 0o600)

    def test_defaults_are_safe(self) -> None:
        plan = ImportPlan()
        self.assertFalse(plan.apply)
        self.assertFalse(plan.start)
        self.assertFalse(plan.force)
        self.assertFalse(plan.with_toolchains)
        self.assertFalse(plan.with_vision)


class ImportPlanValidationTest(unittest.TestCase):
    def test_empty_tenant_fails(self) -> None:
        plan = ImportPlan(tenant="")
        err = plan.validate()
        self.assertIsNotNone(err)

    def test_whitespace_tenant_fails(self) -> None:
        plan = ImportPlan(tenant="   ")
        err = plan.validate()
        self.assertIsNotNone(err)


class ImportPlanCliTest(unittest.TestCase):
    def test_kimi_flag_reaches_plan(self) -> None:
        args = build_parser().parse_args(
            ["import", "--tenant", "testimport", "--with-kimi"]
        )
        self.assertTrue(_plan_from_args(args).with_kimi)

    def test_legacy_nanocode_resume_fails_without_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "plan.json")
            with open(path, "w", encoding="utf-8") as handle:
                json.dump({"with_nanocode": True}, handle)
            with patch("hermes_kawarimi.cli.out") as mock_out:
                result = main(["import", "--resume", path])

        self.assertEqual(result, 2)
        mock_out.assert_called_once_with(
            "[red]error:[/red] invalid import plan: "
            "nanocode worker is no longer supported"
        )


if __name__ == "__main__":
    unittest.main()
