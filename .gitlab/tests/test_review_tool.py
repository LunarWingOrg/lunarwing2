"""WI#741: the open-MR review helper (docs/reviews/tools/review.py).

The helper is what a review run reads first, on whatever harness runs it. Three
things failed silently before and are pinned here: a default target that is a
release behind, a fresh checkout without a state file, and a prefix sequence
that ended at ZZ.
"""

import contextlib
import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
TOOL = ROOT / "docs" / "reviews" / "tools" / "review.py"


def load_tool():
    spec = importlib.util.spec_from_file_location("review_tool_under_test", TOOL)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_tool(*args):
    return subprocess.run(
        [sys.executable, str(TOOL), *args], capture_output=True, text=True, timeout=60, check=False
    )


@unittest.skipUnless(TOOL.is_file(), "docs/reviews/tools/review.py is not in this tree (a partial export)")
class ReviewToolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tool = load_tool()

    def test_prefix_sequence_continues_past_two_letters(self):
        cases = {
            "AA": "AB", "AZ": "BA", "ZY": "ZZ", "ZZ": "AAA",
            "AAA": "AAB", "AES": "AET", "AEZ": "AFA", "AZZ": "BAA", "ZZZ": "AAAA",
        }
        for current, expected in cases.items():
            with self.subTest(current=current):
                self.assertEqual(self.tool.next_prefix(current), expected)

    def test_prefix_sequence_never_repeats(self):
        seen, current = set(), "ZX"
        for _ in range(60):
            self.assertNotIn(current, seen)
            seen.add(current)
            current = self.tool.next_prefix(current)

    def test_malformed_prefix_is_refused(self):
        for bad in ("A", "", "aa", "A1", "A-", "ÄÄ", None, 12):
            with self.subTest(prefix=bad):
                with contextlib.redirect_stderr(io.StringIO()) as stderr:
                    with self.assertRaises(SystemExit) as raised:
                        self.tool.next_prefix(bad)
                self.assertEqual(raised.exception.code, 1)
                self.assertIn("two or more capital letters", stderr.getvalue())

    def test_default_target_is_a_release_branch_and_overridable(self):
        # The built-in default is only a fallback, but it is what a session that
        # "just runs the tool" gets, so it must name a release branch.
        self.assertRegex(self.tool.TARGET_DEFAULT, r"^aragents/kimil00p/v\d+\.\d+\.\d+\.\d+$")
        result = subprocess.run(
            [sys.executable, "-c",
             "import importlib.util, sys;"
             "s = importlib.util.spec_from_file_location('t', sys.argv[1]);"
             "m = importlib.util.module_from_spec(s); s.loader.exec_module(m);"
             "print(m.TARGET_DEFAULT)", str(TOOL)],
            capture_output=True, text=True, timeout=60, check=False,
            env={"PATH": "/usr/bin:/bin:/usr/local/bin", "REVIEW_TARGET": "some/other/branch"},
        )
        self.assertEqual(result.stdout.strip(), "some/other/branch", result.stderr)

    def test_stale_target_is_named_by_open_merge_requests_elsewhere(self):
        target = "aragents/kimil00p/v2.1.8.0"
        others = [
            {"target_branch": "aragents/kimil00p/v2.2.0.0"},
            {"target_branch": "aragents/kimil00p/v2.2.0.0"},
            {"target_branch": "aragents/kimil00p/v2.2.0.0", "draft": True},
            {"target_branch": "master"},
        ]
        warnings = self.tool.target_warnings(
            target, "2026-11-16T00:00:00.000+00:00", "2026-11-16T12:00:00.000+00:00", others
        )
        self.assertEqual(len(warnings), 1, warnings)
        self.assertIn("2 open non-draft MR(s) target aragents/kimil00p/v2.2.0.0", warnings[0])
        self.assertIn(target, warnings[0])

    def test_stale_target_is_named_by_the_age_of_its_head(self):
        target = "aragents/kimil00p/v2.1.8.0"
        old, new = "2026-11-06T10:00:00.000+00:00", "2026-11-17T01:58:00.000+00:00"
        warnings = self.tool.target_warnings(target, old, new, [])
        self.assertEqual(len(warnings), 1, warnings)
        self.assertIn("10 days older than master's", warnings[0])
        # A live target is quiet: a few days behind master, MRs only on master or as drafts.
        quiet = self.tool.target_warnings(
            "aragents/kimil00p/v2.2.0.0", "2026-11-14T00:00:00+00:00", new,
            [{"target_branch": "master"}, {"target_branch": "aragents/kimil00p/v2.2.1.0", "draft": True}],
        )
        self.assertEqual(quiet, [])
        # Dates the API did not give are no reason to fail or to warn.
        self.assertEqual(self.tool.target_warnings(target, None, new, []), [])
        # A target without a slash has no release family to compare the other MRs with.
        self.assertEqual(self.tool.target_warnings("master", new, new, [{"target_branch": "x/y"}]), [])

    def test_init_creates_the_state_file_once(self):
        with tempfile.TemporaryDirectory() as temporary:
            state = pathlib.Path(temporary) / "open-mr-review-state.json"
            first = run_tool("--state", str(state), "init")
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(
                json.loads(state.read_text()),
                {"last_run": 0, "next_prefix": "AA", "mrs": {}, "runs": [],
                 "target_heads": {}, "previous_reports": []},
            )
            prefix = run_tool("--state", str(state), "prefix", "3")
            self.assertEqual(prefix.stdout.strip(), "AA AB AC", prefix.stderr)
            # A state file holds every review's record: init must never replace one.
            state.write_text(json.dumps({"last_run": 458, "next_prefix": "AES", "mrs": {"1": {}}}))
            second = run_tool("--state", str(state), "init")
            self.assertEqual(second.returncode, 3, second.stdout)
            self.assertIn("never overwrites", second.stderr)
            self.assertEqual(json.loads(state.read_text())["last_run"], 458)

    def test_a_missing_state_file_names_the_way_out(self):
        with tempfile.TemporaryDirectory() as temporary:
            state = pathlib.Path(temporary) / "absent.json"
            fragment = pathlib.Path(temporary) / "fragment.json"
            fragment.write_text(json.dumps({"last_run": 1}))
            for args in (("prefix",), ("merge-state", str(fragment), "--force"),
                         ("note-id", "1", "2")):
                with self.subTest(command=args[0]):
                    result = run_tool("--state", str(state), *args)
                    self.assertEqual(result.returncode, 1, result.stdout)
                    self.assertIn("review.py init", result.stderr)
                    self.assertFalse(state.exists())

    def test_the_first_run_after_init_merges_without_force(self):
        with tempfile.TemporaryDirectory() as temporary:
            state = pathlib.Path(temporary) / "state.json"
            fragment = pathlib.Path(temporary) / "fragment.json"
            self.assertEqual(run_tool("--state", str(state), "init").returncode, 0)
            fragment.write_text(json.dumps({
                "last_run": 1, "next_prefix": "AB", "target_head": "a0e1d43b9",
                "mrs": {"7": {"item": "WI#741", "prefix": "AA", "posted_note_ids": [5]}},
                "runs_entry": {"run": 1},
            }))
            merged = run_tool("--state", str(state), "merge-state", str(fragment))
            self.assertEqual(merged.returncode, 0, merged.stderr)
            result = json.loads(state.read_text())
            self.assertEqual((result["last_run"], result["next_prefix"]), (1, "AB"))
            self.assertEqual(result["mrs"]["7"]["item"], "WI#741")
            self.assertEqual(result["target_heads"], {"run1": "a0e1d43b9"})
            again = run_tool("--state", str(state), "merge-state", str(fragment))
            self.assertEqual(again.returncode, 3, again.stdout)


if __name__ == "__main__":
    unittest.main()
