#!/usr/bin/env python3
"""WI#363 (offline half, scope-narrowed 2026-10-20): unit tests for the
ic/deny.toml accepted-advisory guard.

validate_config.validate_deny_toml_advisories (ci-config lane) requires
every [advisories].ignore reason to name a "clearing owner WI#N" and every
cited repo-relative path to be tracked, with a no-git checkout-existence
fallback for the git-less ci-config image. The online owner-liveness half of
MR !1795 was dropped by the scope decision: no owner-liveness job or network
call exists. Registered in .gitlab/scripts/python_tests.py as well, so the
suite also runs in python-unit.
"""

import importlib.util
import io
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_module(name: str, relative: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


config = load_module("validate_config", ".gitlab/scripts/validate_config.py")


class DenyTomlAdvisoryGuardTests(unittest.TestCase):
    """Offline half (WI#363 fix item 1): reason shape + cited-path existence."""

    TRACKED = {
        "ic/deny.toml",
        ".gitlab/README.md",
        "docs/ops/EXISTS.md",
        "ic/src/main.rs",
        ".gitlab/scripts/check-default-graph-advisories.sh",
    }
    DENY_OK = """[advisories]
unmaintained = "workspace"
# Audit trail: docs/ops/EXISTS.md and the module tree under ic/src/.
# Prose fragments like opencode/opencode2/pebble or 2026-0098/0099 are not paths.
ignore = [
    { id = "RUSTSEC-0000-0001", reason = "demo; clearing owner WI#363" },
    { id = "RUSTSEC-0000-0002", reason = "demo two; decision record WI#331 (closed), clearing owner WI#367" },
]
"""
    README_OK = (
        "# CI\n"
        "\n"
        f"{config.README_DENY_GUARD_BEGIN}\n"
        "- cargo-deny gate; see docs/ops/EXISTS.md and docs/ops/EXISTS.md:12-34.\n"
        f"{config.README_DENY_GUARD_END}\n"
        "\n"
        "- outside the markers, docs/ops/GONE.md is not checked.\n"
    )

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)

    def errors(self, deny_text: str | None, readme_text: str | None) -> list[str]:
        if deny_text is not None:
            deny_dir = self.root / "ic"
            deny_dir.mkdir(parents=True, exist_ok=True)
            (deny_dir / "deny.toml").write_text(deny_text, encoding="utf-8")
        if readme_text is not None:
            readme_dir = self.root / ".gitlab"
            readme_dir.mkdir(parents=True, exist_ok=True)
            (readme_dir / "README.md").write_text(readme_text, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", self.root):
            config.validate_deny_toml_advisories(errors, tracked=set(self.TRACKED))
        return errors

    def ok(self) -> list[str]:
        return self.errors(self.DENY_OK, self.README_OK)

    def test_real_tree_passes(self):
        # The checked-in ic/deny.toml + .gitlab/README.md must satisfy the
        # guard (uses the real git ls-files, not a fixture set).
        errors: list[str] = []
        config.validate_deny_toml_advisories(errors)
        self.assertEqual(errors, [])

    def test_fixture_passes(self):
        self.assertEqual(self.ok(), [])

    def test_entry_without_reason_fails(self):
        deny = '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001" } ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("RUSTSEC-0000-0001 has no reason" in error for error in errors))

    def test_empty_reason_fails(self):
        deny = '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", reason = "  " } ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("has no reason" in error for error in errors))

    def test_reason_without_wi_fails(self):
        deny = '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", reason = "no reference" } ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("names no WI#N work item" in error for error in errors))

    def test_reason_with_wi_but_no_clearing_owner_fails(self):
        # A reason naming only a (closed) historical decision record names
        # no owner to re-verify the acceptance against — refused offline
        # (WI#363; owner liveness itself is out of scope by the 2026-10-20
        # scope decision, so the requirement is the phrase, not openness).
        deny = (
            '[advisories]\nignore = [ { id = "RUSTSEC-0000-0001", '
            'reason = "decision record WI#331" } ]\n'
        )
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("names no 'clearing owner WI#N'" in error for error in errors))

    def test_non_table_entry_fails(self):
        deny = '[advisories]\nignore = [ "RUSTSEC-0000-0001" ]\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("entry 0 must be a table" in error for error in errors))

    def test_ignore_not_a_list_fails(self):
        deny = '[advisories]\nignore = "RUSTSEC-0000-0001"\n'
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("[advisories].ignore must be a list" in error for error in errors))

    def test_unparseable_toml_fails(self):
        errors = self.errors("not [ toml", self.README_OK)
        self.assertTrue(any("does not parse" in error for error in errors))

    def test_missing_deny_toml_fails(self):
        errors = self.errors(None, self.README_OK)
        self.assertTrue(any("cannot read ic/deny.toml" in error for error in errors))

    def test_missing_path_in_deny_comment_fails(self):
        deny = self.DENY_OK.replace("docs/ops/EXISTS.md", "docs/ops/GONE.md", 1)
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(
            any(
                "ic/deny.toml cites docs/ops/GONE.md" in error for error in errors
            )
        )

    def test_missing_path_in_readme_section_fails(self):
        readme = self.README_OK.replace("docs/ops/EXISTS.md:12-34", "docs/ops/GONE.md:12-34")
        errors = self.errors(self.DENY_OK, readme)
        self.assertTrue(
            any(
                ".gitlab/README.md cargo-deny section cites docs/ops/GONE.md" in error
                for error in errors
            )
        )

    def test_directory_citation_must_exist(self):
        deny = self.DENY_OK.replace("ic/src/", "ic/nope/")
        errors = self.errors(deny, self.README_OK)
        self.assertTrue(any("ic/nope" in error for error in errors))

    def test_missing_readme_markers_fail(self):
        errors = self.errors(self.DENY_OK, "# CI\n- no markers here\n")
        self.assertTrue(any("marker pair" in error for error in errors))

    def test_prose_fragments_are_not_citations(self):
        # The fixture comments carry opencode/opencode2/pebble, 2026-0098/0099
        # and WI#331/WI#409-shaped fragments; none may be flagged.
        self.assertEqual(self.ok(), [])


class DenyTomlNoGitFallbackTests(unittest.TestCase):
    """The ci-config lint image (python:3.12-slim) ships no git binary, so
    `git ls-files` raises FileNotFoundError there (pipeline 5929, job
    117472). The guard must fall back to checkout existence, not crash."""

    def no_git(self):
        return mock.patch.object(
            config.subprocess, "run", side_effect=FileNotFoundError("git")
        )

    def test_real_tree_passes_without_git(self):
        errors: list[str] = []
        with self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(errors, [])

    def test_missing_path_still_fails_without_git(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name)
        (root / "docs" / "ops").mkdir(parents=True)
        (root / "docs" / "ops" / "EXISTS.md").write_text("x", encoding="utf-8")
        (root / "ic" / "src").mkdir(parents=True)
        (root / "ic" / "src" / "main.rs").write_text("", encoding="utf-8")
        (root / ".gitlab").mkdir()
        (root / ".gitlab" / "README.md").write_text(
            DenyTomlAdvisoryGuardTests.README_OK, encoding="utf-8"
        )
        deny = (
            DenyTomlAdvisoryGuardTests.DENY_OK
            + "# stale citation: docs/ops/GONE.md"
            + chr(10)
        )
        (root / "ic" / "deny.toml").write_text(deny, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", root), self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("docs/ops/GONE.md", errors[0])

    def test_citation_escaping_the_checkout_fails_without_git(self):
        # git mode refuses all three citations (none is a tracked path); the
        # fallback must not accept them just because the target exists
        # (run-157 KF-1).
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        outer = pathlib.Path(temporary.name)
        (outer / "outside.md").write_text("x", encoding="utf-8")
        root = outer / "repo"
        (root / "ic" / "src").mkdir(parents=True)
        (root / "ic" / "src" / "main.rs").write_text("", encoding="utf-8")
        (root / "docs" / "ops").mkdir(parents=True)
        (root / "docs" / "ops" / "EXISTS.md").write_text("x", encoding="utf-8")
        (root / "docs" / "escape").symlink_to(outer, target_is_directory=True)
        (root / ".gitlab").mkdir()
        (root / ".gitlab" / "README.md").write_text(
            DenyTomlAdvisoryGuardTests.README_OK, encoding="utf-8"
        )
        deny = (
            DenyTomlAdvisoryGuardTests.DENY_OK
            + "# traversal: ic/../../outside.md"
            + chr(10)
            + "# symlink: docs/escape/outside.md"
            + chr(10)
            + "# in-repo traversal: ic/../ic/src/main.rs"
            + chr(10)
        )
        (root / "ic" / "deny.toml").write_text(deny, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", root), self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 3, errors)
        joined = chr(10).join(errors)
        self.assertIn("ic/../../outside.md", joined)
        self.assertIn("docs/escape/outside.md", joined)
        self.assertIn("ic/../ic/src/main.rs", joined)

    def test_git_failure_is_an_error_not_a_fallback(self):
        # A `git ls-files` that fails while git is present (for example,
        # dubious ownership) is an error, not a silent fallback (run-157
        # KF-2, mutant M5).
        failed = config.subprocess.CompletedProcess(
            ["git", "ls-files"], 128, "", "fatal: not a git repository"
        )
        errors: list[str] = []
        with mock.patch.object(config.subprocess, "run", return_value=failed):
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("git ls-files failed", errors[0])

    def test_citation_through_a_directory_symlink_fails_without_git(self):
        # git mode lists a directory symlink itself, never the paths beneath
        # it (e.g. ic/channels-src/darkirc -> ../../darkirc_channel_for_lunarwing/
        # darkirc), so the fallback must refuse such citations even when the
        # target resolves inside the checkout (run-161 KN-2). A symlink at
        # the final component stays accepted because git lists leaf symlinks.
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name)
        (root / "ic" / "src").mkdir(parents=True)
        (root / "ic" / "src" / "main.rs").write_text("", encoding="utf-8")
        (root / "docs" / "ops").mkdir(parents=True)
        (root / "docs" / "ops" / "EXISTS.md").write_text("x", encoding="utf-8")
        (root / "ic" / "realdir").mkdir()
        (root / "ic" / "realdir" / "file.md").write_text("x", encoding="utf-8")
        (root / "ic" / "linkdir").symlink_to("realdir", target_is_directory=True)
        (root / "ic" / "realfile.md").write_text("x", encoding="utf-8")
        (root / "ic" / "leaf.md").symlink_to("realfile.md")
        (root / ".gitlab").mkdir()
        (root / ".gitlab" / "README.md").write_text(
            DenyTomlAdvisoryGuardTests.README_OK, encoding="utf-8"
        )
        deny = (
            DenyTomlAdvisoryGuardTests.DENY_OK
            + "# through a directory symlink: ic/linkdir/file.md"
            + chr(10)
            + "# leaf symlink: ic/leaf.md"
            + chr(10)
            + "# plain file: ic/realfile.md"
            + chr(10)
        )
        (root / "ic" / "deny.toml").write_text(deny, encoding="utf-8")
        errors: list[str] = []
        with mock.patch.object(config, "ROOT", root), self.no_git():
            config.validate_deny_toml_advisories(errors)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("ic/linkdir/file.md", errors[0])


class DenyTomlMainWiringTests(unittest.TestCase):
    """The ci-config lane runs validate_config.py's main(), so the offline
    guard only protects anything while main() calls it."""

    def test_main_runs_the_deny_toml_guard(self):
        calls = []

        def record(errors, tracked=None):
            calls.append(tracked)
            errors.append("WI#363 wiring probe")

        with (
            mock.patch.object(
                config, "validate_deny_toml_advisories", side_effect=record
            ),
            mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            self.assertEqual(config.main(), 1)
        self.assertEqual(calls, [None])
        self.assertIn("WI#363 wiring probe", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
