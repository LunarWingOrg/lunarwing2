"""Unit tests for CLI argument -> ImportPlan wiring.

No root or DB needed — these test parser/plan plumbing only.
"""

from __future__ import annotations

import unittest

from hermes_kawarimi.cli import _plan_from_args, build_parser


def _plan(argv: list[str]):
    args = build_parser().parse_args(argv)
    return _plan_from_args(args)


class PlanFromArgsTest(unittest.TestCase):
    def test_worker_flags_wire_through(self) -> None:
        plan = _plan(
            [
                "import",
                "--tenant",
                "kageho",
                "--with-nanocode",
                "--with-pebble",
                "--with-opencode",
                "--with-kimi",
                "--with-toolchains",
            ]
        )
        self.assertTrue(plan.with_nanocode)
        self.assertTrue(plan.with_pebble)
        self.assertTrue(plan.with_opencode)
        self.assertTrue(plan.with_kimi)
        self.assertTrue(plan.with_toolchains)

    def test_defaults_are_safe(self) -> None:
        plan = _plan(["import", "--tenant", "kageho"])
        self.assertFalse(plan.apply)
        self.assertFalse(plan.start)
        self.assertFalse(plan.with_kimi)

    def test_apply_and_start(self) -> None:
        plan = _plan(["import", "--tenant", "kageho", "--apply", "--start"])
        self.assertTrue(plan.apply)
        self.assertTrue(plan.start)


if __name__ == "__main__":
    unittest.main()
