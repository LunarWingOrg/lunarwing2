#!/usr/bin/env python3
"""Regression checks for the built-image evaluation's fail-closed verdicts."""

import copy
import unittest

from console_policy_eval import _console_policy_verdict, _integration_policy_verdict


class IntegrationPolicyVerdictTests(unittest.TestCase):
    def setUp(self):
        self.baseline = {"providerIDs": ["tensorzero"], "websearchIDs": ["exa"]}
        self.control = {
            "consolePolicyDeniesIntegration": "*", "providerIDs": [], "websearchIDs": [],
        }
        self.guarded = {**self.baseline, "consolePolicyDeniesIntegration": "*"}

    def verdict(self):
        return _integration_policy_verdict(
            self.control, self.guarded, self.baseline, "tensorzero", "*",
        )[0]

    def test_guarded_catalogs_unchanged(self):
        self.assertEqual(self.verdict(), 0)

    def test_provider_specific_deny(self):
        for run in (self.control, self.guarded):
            run["consolePolicyDeniesIntegration"] = "tensorzero*"
        self.control["websearchIDs"] = ["exa"]
        self.assertEqual(_integration_policy_verdict(
            self.control, self.guarded, self.baseline, "tensorzero", "tensorzero*",
        )[0], 0)

    def test_regressed_policy_removes_provider(self):
        self.guarded["providerIDs"] = []
        self.assertEqual(self.verdict(), 1)

    def test_regressed_policy_removes_search(self):
        self.guarded["websearchIDs"] = []
        self.assertEqual(self.verdict(), 1)

    def test_wrong_statement_order_admits_console(self):
        self.guarded["consoleProviders"] = ["wi665-console-provider"]
        self.assertEqual(self.verdict(), 1)

    def test_vacuous_controls_are_inconclusive(self):
        for key, value in (("providerIDs", ["tensorzero"]), ("websearchIDs", ["exa"]),
                           ("consolePolicyDeniesIntegration", "")):
            with self.subTest(key=key):
                original = copy.deepcopy(self.control)
                self.control[key] = value
                self.assertEqual(self.verdict(), 2)
                self.control = original

    def test_empty_baseline_is_inconclusive(self):
        self.baseline["websearchIDs"] = []
        self.assertEqual(self.verdict(), 2)


class ToolPermissionVerdictTests(unittest.TestCase):
    def setUp(self):
        self.control = {"consoleToolEffect": "deny", "providerIDs": []}
        self.baseline = {"providerIDs": ["tensorzero"]}
        self.guarded = {
            "consolePolicyDeniesProvider": "tensorzero", "providerIDs": ["tensorzero"],
            "consoleToolEffect": "allow", "consoleToolAskEffect": "ask",
            "consoleToolDenyEffect": "deny",
        }

    def verdict(self):
        return _console_policy_verdict(self.control, self.baseline, self.guarded, "tensorzero")[0]

    def test_ordinary_permissions_preserved(self):
        self.assertEqual(self.verdict(), 0)

    def test_lifting_ordinary_deny_fails(self):
        self.guarded["consoleToolDenyEffect"] = "allow"
        self.assertEqual(self.verdict(), 1)

    def test_lifting_ask_fails(self):
        self.guarded["consoleToolAskEffect"] = "allow"
        self.assertEqual(self.verdict(), 1)


if __name__ == "__main__":
    unittest.main()
