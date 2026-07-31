"""CLI forwarding tests. No root or DB required."""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from unittest.mock import patch

from hermes_kawarimi.cli import _plan_from_args, build_parser, main


class ImportWorkerFlagsTest(unittest.TestCase):
    def test_with_kimi_and_toolchains_are_forwarded(self) -> None:
        args = build_parser().parse_args(
            [
                "import",
                "--tenant",
                "alpha",
                "--with-kimi",
                "--with-toolchains",
            ]
        )

        plan = _plan_from_args(args)

        self.assertTrue(plan.with_kimi)
        self.assertTrue(plan.with_toolchains)


class LegacyPlanRejectionTest(unittest.TestCase):
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
