"""Unit tests for loader wiring (flag construction, schema timeout).

No root or DB needed — these test the pure helper functions.
"""

from __future__ import annotations

import os
import unittest
from unittest import mock

from hermes_kawarimi.config import ImportPlan
from hermes_kawarimi.loader import _build_flags, _worker_flags, _schema_wait_seconds


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


if __name__ == "__main__":
    unittest.main()
