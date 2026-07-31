"""CLI forwarding tests. No root or DB required."""

from __future__ import annotations

import unittest

from hermes_kawarimi.cli import _plan_from_args, build_parser


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


if __name__ == "__main__":
    unittest.main()
