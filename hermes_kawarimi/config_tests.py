"""Unit tests for ImportPlan serialization and validation. No root/DB."""

from __future__ import annotations

import json
import os
import tempfile
import unittest

from hermes_kawarimi.config import ImportPlan


class ImportPlanSerializationTest(unittest.TestCase):
    def test_roundtrip_preserves_all_fields(self) -> None:
        plan = ImportPlan(
            source="/tmp/hermes",
            tenant="testimport",
            apply=True,
            start=True,
            force=True,
            with_kimi=True,
            with_toolchains=True,
            with_vision=False,
            tensormoon_url="http://tz:3000",
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
        self.assertTrue(loaded.with_kimi)
        self.assertTrue(loaded.with_toolchains)
        self.assertFalse(loaded.with_vision)
        self.assertEqual(loaded.tensormoon_url, "http://tz:3000")
        self.assertEqual(loaded.llm_model, "claude-sonnet")

    def test_legacy_tensorzero_url_plan_key_is_read(self) -> None:
        loaded = ImportPlan.from_dict({"tenant": "legacy", "tensorzero_url": "http://tz:3000"})
        self.assertEqual(loaded.tensormoon_url, "http://tz:3000")
        data = loaded.to_dict()
        self.assertEqual(data["tensormoon_url"], "http://tz:3000")
        self.assertNotIn("tensorzero_url", data)

    def test_tensormoon_url_plan_key_wins_over_legacy(self) -> None:
        loaded = ImportPlan.from_dict(
            {"tenant": "both", "tensormoon_url": "http://new:3000", "tensorzero_url": "http://old:3000"}
        )
        self.assertEqual(loaded.tensormoon_url, "http://new:3000")

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


if __name__ == "__main__":
    unittest.main()
